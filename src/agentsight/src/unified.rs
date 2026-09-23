//! AgentSight - Unified entry point for AI Agent observability
//!
//! This module provides the main `AgentSight` struct that orchestrates the entire
//! data pipeline: probes → parser → aggregator → analyzer → storage.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                            AgentSight                                │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │   probes     parser    aggregator    analyzer    genai    storage    │
//! │     ↓          ↓           ↓            ↓          ↓        ↓       │
//! │   Event   ParsedMessage  Aggregated   Analysis  Semantic  持久化    │
//! │                          Result       Result    Events              │
//! │                                                  ↓                  │
//! │                                            GenAI Storage            │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::aggregator::Aggregator;
use crate::analyzer::Analyzer;
use crate::config::{AgentsightConfig, StorageConfig};
use crate::discovery::AgentScanner;
use crate::event::Event;
use crate::ffi::FfiEventSender;
use crate::genai::semantic::GenAISemanticEvent;
use crate::genai::{GenAIBuilder, GenAIExporter, LogtailExporter};
use crate::interruption::{
    DetectorConfig, InterruptionDetector, ProcessExitStatus, recover_oom_events,
};
use crate::parser::Parser;
use crate::probes::{ChannelWatermarks, FileWatchEvent, FileWriteEvent, Probes, ProbesPoller};
use crate::response_map::ResponseSessionMapper;
use crate::storage::sqlite::{GenAISqliteStore, InterruptionStore};
use crate::storage::{SqliteConfig, Storage, TimePeriod, TokenQuery, TokenQueryResult};
use crate::tokenizer::LlmTokenizer;

/// Main AgentSight struct for tracing AI agent activity
///
/// This is the unified entry point that orchestrates:
/// - `Probes`: eBPF-based event capture
/// - `Parser`: Message parsing
/// - `Aggregator`: Event aggregation
/// - `Analyzer`: Analysis and record extraction
/// - `Storage`: Persistence
/// - `AgentScanner`: Process lifecycle tracking
pub struct AgentSight {
    /// BPF probes manager
    probes: Probes,
    /// Message parser (unified)
    parser: Parser,
    /// Event aggregator (unified)
    aggregator: Aggregator,
    /// Unified analyzer
    analyzer: Analyzer,
    /// GenAI semantic builder
    genai_builder: GenAIBuilder,
    /// Pluggable GenAI event exporters (JSONL, SLS, etc.)
    genai_exporters: Vec<Box<dyn GenAIExporter>>,
    /// Direct reference to the SQLite GenAI store for two-phase pending/complete writes.
    /// `None` when SLS is configured (SQLite exporter is not registered in that case).
    genai_sqlite_store: Option<Arc<GenAISqliteStore>>,
    /// Interruption event detector (online rules)
    interruption_detector: InterruptionDetector,
    /// Interruption event store (SQLite)
    interruption_store: Option<Arc<InterruptionStore>>,
    /// Unified storage
    storage: Storage,
    /// Agent scanner for process lifecycle tracking
    scanner: AgentScanner,
    /// Poller handle
    _poller: ProbesPoller,
    /// Running flag
    running: Arc<AtomicBool>,
    /// Event counter
    event_count: u64,
    /// File watch callback for .jsonl file open events
    filewatch_callback: Option<Box<dyn Fn(FileWatchEvent) + Send + 'static>>,
    /// ResponseId → SessionId mapper for FileWrite events
    response_mapper: ResponseSessionMapper,
    /// Pending GenAI events awaiting session_id resolution from ResponseSessionMapper
    pending_genai: Vec<PendingGenAI>,
    /// Calls exported with a fallback session_id after the deferral window
    /// timed out, keyed by pid, awaiting a late FileWrite mapping for
    /// retroactive session fix-up (issue #2059).
    retro_session_fixup: lru::LruCache<u32, Vec<RetroFixupEntry>>,
    /// Total estimated bytes of all pending_genai entries (for memory budget enforcement).
    pending_genai_bytes: usize,
    /// Runtime limits for bounded buffers and eviction policies.
    runtime_limits: crate::config::RuntimeLimits,
    /// Optional FFI event sender (set when running in FFI/C-API mode)
    ffi_sender: Option<FfiEventSender>,
    /// Rate-limiter for dead-PID connection drain (at most once per second)
    last_drain_check: std::time::Instant,
    /// Rate-limiter for interruption DB purge (at most once per 60 s)
    last_interruption_purge: std::time::Instant,
    /// Rate-limiter for the buffer watermark log (at most once per 60 s)
    last_watermark_log: std::time::Instant,
    /// Cache of pid → agent_name, persists after process exit for deferred resolution
    pid_agent_name_cache: lru::LruCache<u32, String>,
    /// Live Agent processes sampled for Session resource timelines.
    resource_targets: Arc<RwLock<HashMap<u32, String>>>,
    /// Resource sampler joined before the GenAI database is checkpointed.
    resource_sampler_handle: Option<std::thread::JoinHandle<()>>,
    /// HTTP domain patterns from config, used for runtime DNS-based tcpsniff target addition
    http_domains: Vec<String>,
    /// Mailbox for watcher thread to deposit a dynamically-created LogtailExporter
    pending_logtail: Arc<Mutex<Option<Box<dyn GenAIExporter>>>>,
    /// DeadLoop auto-kill: enabled flag
    deadloop_kill_enabled: bool,
    /// DeadLoop auto-kill: trigger threshold (kill after N detections)
    deadloop_kill_after_count: usize,
    /// Process killer for DeadLoop auto-kill (injectable for tests)
    process_killer: Arc<dyn crate::utils::process::ProcessKiller>,
    /// Cached feature flags so runtime paths can check them without the config.
    features: crate::config::FeatureFlags,
    /// Effective SQLite lifecycle policies.
    storage_config: StorageConfig,
}

/// GenAI events waiting for session_id resolution via ResponseSessionMapper.
/// If the mapper lookup succeeds within the timeout, session_id metadata is updated
/// before export. Otherwise, the events are exported with the response_id-based
/// fallback (`SHA256("session" + first_response_id)`).
struct PendingGenAI {
    events: Vec<GenAISemanticEvent>,
    response_id: String,
    pid: u32,
    created_at: std::time::Instant,
}

impl PendingGenAI {
    /// Rough byte estimate for memory budget enforcement.
    fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.response_id.len() + self.events.len() * 512 // conservative per-event estimate
    }
}

/// Maximum time to wait for ResponseSessionMapper to resolve a session_id
const PENDING_SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a timeout-escaped call stays eligible for retroactive session
/// fix-up after export (see `apply_retro_session_fixup`). Generous compared
/// to `PENDING_SESSION_TIMEOUT` because the write that reveals the session
/// UUID may only happen at the end of a long agent turn.
const RETRO_FIXUP_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// Maximum number of pids tracked for retroactive session fix-up.
const RETRO_FIXUP_CAPACITY: usize = 256;

/// One timeout-escaped call awaiting retroactive session fix-up:
/// (call_id, registration time, agent_name of the pid at registration time).
/// The agent name guards against pid reuse (see `apply_retro_session_fixup`).
type RetroFixupEntry = (String, std::time::Instant, Option<String>);

impl AgentSight {
    /// Create a new AgentSight instance from configuration
    ///
    /// # Arguments
    /// * `config` - AgentsightConfig containing all configuration parameters
    ///
    /// # Example
    /// ```rust,ignore
    /// use agentsight::{AgentSight, AgentsightConfig};
    ///
    /// let config = AgentsightConfig::new();
    /// let mut sight = AgentSight::new(config)?;
    /// ```
    pub fn new(mut config: AgentsightConfig) -> Result<Self> {
        // Load rules from config file only when config_path is set (CLI --config)
        // FFI users provide rules via API, no config file needed.
        let mut config_load_ok = false;
        let mut config_load_err: Option<(PathBuf, anyhow::Error)> = None;
        if let Some(path) = config.config_path.clone() {
            // ensure_default_agents_config handles both cases:
            // - File missing: creates it with the embedded default.
            // - File exists: checks schema_version; if outdated, backs up and
            //   overwrites with the current default.
            let load_result = match crate::config::ensure_default_agents_config(&path) {
                Ok(()) => config.load_from_file(&path),
                Err(e) => Err(e),
            };
            match load_result {
                Ok(()) => config_load_ok = true,
                Err(e) => {
                    config_load_err = Some((path, e));
                    config.cmdline_rules = crate::config::default_cmdline_rules();
                }
            }
        }

        // Init logging after config file load so JSON `log_path` / `verbose` apply.
        config.apply_verbose();

        // Establish the procfs root before anything resolves a pid through it,
        // and before a probe bakes the pid-namespace flag derived from it into
        // its skeleton.
        crate::utils::procfs::configure(&config.procfs_root);

        if config_load_ok {
            if let Some(path) = config.config_path.as_ref() {
                log::info!(
                    "Loaded {} cmdline rule(s), {} https rule(s), {} http target(s) from {:?}",
                    config.cmdline_rules.len(),
                    config.https_rules.len(),
                    config.http_targets.len(),
                    path
                );
            }
        } else if let Some((path, e)) = config_load_err {
            log::warn!("Failed to load config from {path:?}: {e}, using embedded defaults");
        }

        // Persistence check runs after the config file load so the warning
        // targets the storage directory actually in effect.
        crate::container::warn_if_data_dir_not_persistent(&config.storage.base_path);

        let all_cmdline_rules = config.cmdline_rules.clone();

        // Derive tcp_targets from http_targets (endpoint entries only)
        let mut tcp_targets: Vec<crate::config::TcpTarget> = config
            .http_targets
            .iter()
            .filter_map(|t| match t {
                crate::config::HttpTarget::Endpoint(ep) => Some(ep.clone()),
                crate::config::HttpTarget::Domain(_) => None,
            })
            .collect();

        // Collect http domain patterns for DNS-based resolution
        let http_domains: Vec<String> = config
            .http_targets
            .iter()
            .filter_map(|t| match t {
                crate::config::HttpTarget::Domain(d) => Some(d.clone()),
                crate::config::HttpTarget::Endpoint(_) => None,
            })
            .collect();

        // Startup DNS resolve: exact http domains → IPs → append to tcp_targets
        for domain in &http_domains {
            if domain.contains('*') || domain.contains('?') || domain.contains('[') {
                continue;
            }
            use std::net::ToSocketAddrs;
            match (domain.as_str(), 0u16).to_socket_addrs() {
                Ok(addrs) => {
                    for addr in addrs {
                        if let std::net::IpAddr::V4(ipv4) = addr.ip() {
                            log::info!("http domain resolve: {domain} → {ipv4}");
                            tcp_targets.push(crate::config::TcpTarget {
                                ip: Some(ipv4),
                                port: None,
                            });
                        }
                    }
                }
                Err(e) => {
                    log::warn!("http domain resolve failed for {domain}: {e}");
                }
            }
        }

        // Create probes - agent discovery is handled by AgentScanner via ProcMon events
        let enable_udpdns = !config.https_rules.is_empty() || !http_domains.is_empty();
        let mut probes = Probes::new_with_cgroup_filter(
            &[],
            config.target_uid,
            config.enable_filewatch,
            enable_udpdns,
            &tcp_targets,
            config.cgroup_filter_enabled,
            &config.runtime_limits,
        )
        .context("Failed to create probes")?;

        // Attach procmon for process monitoring
        probes.attach().context("Failed to attach probes")?;

        // Seed cgroup_filter map with pre-configured cgroup inode IDs
        if config.cgroup_filter_enabled && !config.cgroup_ids.is_empty() {
            for &cg_id in &config.cgroup_ids {
                probes
                    .add_traced_cgroup(cg_id)
                    .context("Failed to register cgroup_id")?;
                log::info!("Registered cgroup_id {cg_id}");
            }
        }

        // Create scanner with all rules (allow/deny/https)
        let mut scanner = AgentScanner::from_rules(&all_cmdline_rules, &config.https_rules);
        let existing_agents = scanner.scan();

        // Attach SSL probes to already-running agents
        for agent in &existing_agents {
            Self::attach_process_internal(&mut probes, agent.pid, &agent.agent_info.name);
        }

        // Connection scan: find processes with established connections to https_rules IPs
        let already_traced: HashSet<u32> = existing_agents.iter().map(|a| a.pid).collect();
        let conn_results = if scanner.has_domain_rules() {
            let conn_scanner = crate::discovery::ConnectionScanner::new(&scanner);
            conn_scanner.scan(&already_traced)
        } else {
            Vec::new()
        };

        // Build pid → agent_name cache from existing agents (persists after process exit)
        let pid_cache_cap =
            std::num::NonZeroUsize::new(config.runtime_limits.pid_cache_size.max(1)).unwrap();
        let mut pid_agent_name_cache = lru::LruCache::new(pid_cache_cap);
        for agent in &existing_agents {
            pid_agent_name_cache.put(agent.pid, agent.agent_info.name.clone());
        }
        for result in &conn_results {
            // Agent name comes from a config rule, else the process comm — never
            // the endpoint domain, never a per-event thread name.
            let agent_name = Self::conn_scan_agent_name(&scanner, result.pid);
            if let Some(ref name) = agent_name {
                pid_agent_name_cache.put(result.pid, name.clone());
            }
            log::debug!(
                "Connection scan: pid={} connected to {}, agent_name={:?}",
                result.pid,
                result.domain,
                agent_name
            );
            Self::attach_process_internal(
                &mut probes,
                result.pid,
                agent_name.as_deref().unwrap_or("unknown"),
            );
        }
        if !conn_results.is_empty() {
            log::info!(
                "Connection scan: attached {} process(es) via established connections",
                conn_results.len()
            );
        }

        // Start polling (non-blocking)
        let _poller = probes.run().context("Failed to start probe poller")?;

        // Initialize unified storage based on config.  When sqlite_storage is
        // disabled we still create an empty storage object so the rest of the
        // pipeline can call storage methods without Option plumbing.
        let storage = if config.features.sqlite_storage_enabled {
            Self::create_storage(&config)?
        } else {
            Storage::noop()?
        };

        // Build GenAI exporters based on feature flags.
        let mut genai_exporters: Vec<Box<dyn GenAIExporter>> = Vec::new();
        let mut genai_sqlite_store: Option<Arc<GenAISqliteStore>> = None;
        let sls_activated = Arc::new(AtomicBool::new(config.features.sls_logtail_enabled));

        // If config has runtime.sls_logtail_path set, seed the dynamic path
        if let Some(ref sls_path) = config.sls_logtail_path {
            crate::genai::logtail::set_dynamic_logtail_path(sls_path);
        }

        // Determine if Logtail is currently enabled (env var OR dynamic config)
        let logtail_currently_enabled = crate::genai::logtail::logtail_enabled();

        // Sysom production mode: when the active Logtail path points under /var/sysom/,
        // use only the external Logtail path. Skip SQLite and the default SLS exporter.
        let sysom_logtail_path =
            crate::genai::logtail::logtail_path().filter(|p| p.starts_with("/var/sysom/"));

        if let Some(ref path) = sysom_logtail_path {
            log::info!(
                "SLS sysom mode detected (path={}), skipping SQLite and default SLS exporter",
                path
            );
            if logtail_currently_enabled {
                let exporter = LogtailExporter::new_with_fixed_path(
                    path,
                    config.encryption_public_key.as_deref(),
                    config.trace_enabled,
                );
                let uid = crate::genai::instance_id::get_owner_account_id();
                if uid.is_empty() {
                    anyhow::bail!(
                        "SLS Logtail exporter is enabled but failed to \
                         fetch owner-account-id from ECS metadata service. \
                         Cannot upload logs without uid. Aborting."
                    );
                }
                log::info!(
                    "Logtail file exporter enabled (sysom, {}), uid={}",
                    exporter.path().display(),
                    uid
                );
                genai_exporters.push(Box::new(exporter));
                sls_activated.store(true, Ordering::SeqCst);
            }
        } else {
            // Default/dev mode: SQLite + default SLS exporter, plus optional env Logtail.
            if config.features.sqlite_storage_enabled {
                let db_path = config.storage.genai_path();
                match GenAISqliteStore::new_with_path_and_batch(
                    &db_path,
                    config.features.sqlite_batch,
                    config.storage.genai,
                ) {
                    Ok(store) => {
                        log::info!(
                            "SQLite GenAI exporter enabled (batch={:?})",
                            config.features.sqlite_batch
                        );
                        let store = Arc::new(store);
                        genai_sqlite_store = Some(Arc::clone(&store));
                        genai_exporters.push(Box::new(store));
                    }
                    Err(e) => {
                        log::warn!("Failed to initialize SQLite GenAI exporter: {e}");
                    }
                }
            }

            // Default local SLS Logtail exporter
            let default_exporter = LogtailExporter::new_default(
                config.encryption_public_key.as_deref(),
                config.trace_enabled,
            );
            log::info!(
                "Default Logtail file exporter enabled ({})",
                default_exporter.path().display()
            );
            genai_exporters.push(Box::new(default_exporter));

            // Also honor explicit SLS_LOGTAIL_FILE if set to a non-sysom path
            if logtail_currently_enabled {
                if let Some(exporter) = LogtailExporter::new(
                    config.encryption_public_key.as_deref(),
                    config.trace_enabled,
                ) {
                    let uid = crate::genai::instance_id::get_owner_account_id();
                    if uid.is_empty() {
                        anyhow::bail!(
                            "SLS Logtail exporter is enabled (SLS_LOGTAIL_FILE set) but failed to \
                             fetch owner-account-id from ECS metadata service. \
                             Cannot upload logs without uid. Aborting."
                        );
                    }
                    log::info!(
                        "Logtail file exporter enabled ({}), uid={}",
                        exporter.path().display(),
                        uid
                    );
                    genai_exporters.push(Box::new(exporter));
                    sls_activated.store(true, Ordering::SeqCst);
                }
            }
        }

        // Create analyzer with tokenizer if configured
        let analyzer = if let Some(ref tokenizer_path) = config.tokenizer_path {
            if Path::new(tokenizer_path).exists() {
                // Assume tokenizer_config.json is in the same directory
                let config_path = Path::new(tokenizer_path)
                    .parent()
                    .map(|p| p.join("tokenizer_config.json"))
                    .unwrap_or_else(|| Path::new("tokenizer_config.json").to_path_buf());

                match LlmTokenizer::from_file(tokenizer_path, &config_path) {
                    Ok(tokenizer) => {
                        log::info!("Tokenizer loaded from: {tokenizer_path:?}");
                        Analyzer::with_tokenizer(tokenizer.clone(), tokenizer)
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to load tokenizer from {tokenizer_path:?}: {e}. Using analyzer without tokenizer."
                        );
                        Analyzer::new()
                    }
                }
            } else {
                log::warn!(
                    "Tokenizer file not found: {tokenizer_path:?}. Using analyzer without tokenizer."
                );
                Analyzer::new()
            }
        } else {
            Analyzer::new()
        };

        // Initialize interruption store only when interruption detection is enabled.
        let interruption_store: Option<Arc<InterruptionStore>> =
            if config.features.interruption_detection_enabled {
                let db_path = config.storage.interruption_path();
                match InterruptionStore::new_with_path(&db_path) {
                    Ok(store) => {
                        log::info!("Interruption events store initialized at {db_path:?}");
                        Some(Arc::new(store))
                    }
                    Err(e) => {
                        log::warn!("Failed to initialize interruption store: {e}");
                        None
                    }
                }
            } else {
                log::debug!("Interruption detection disabled; skipping interruption store");
                None
            };

        // Run OOM recovery: scan dmesg for OOM kill events that occurred while
        // AgentSight was down (e.g. if AgentSight itself was OOM-killed).
        if let Some(ref istore) = interruption_store {
            recover_oom_events(istore, genai_sqlite_store.as_ref(), 0);
        }

        log::info!(
            "AgentSight initialized: {} existing agent(s), {} GenAI exporter(s)",
            existing_agents.len(),
            genai_exporters.len(),
        );

        // Shared mailbox for dynamic LogtailExporter activation
        let pending_logtail: Arc<Mutex<Option<Box<dyn GenAIExporter>>>> =
            Arc::new(Mutex::new(None));

        // Create `running` flag early so background threads can observe shutdown.
        let running = Arc::new(AtomicBool::new(true));
        let resource_targets = Arc::new(RwLock::new(
            existing_agents
                .iter()
                .map(|agent| (agent.pid, agent.agent_info.name.clone()))
                .chain(conn_results.iter().filter_map(|result| {
                    Self::conn_scan_agent_name(&scanner, result.pid).map(|name| (result.pid, name))
                }))
                .collect(),
        ));
        for agent in &existing_agents {
            Self::register_resource_targets(&resource_targets, agent.pid, &agent.agent_info.name);
        }
        for result in &conn_results {
            if let Some(agent_name) = Self::conn_scan_agent_name(&scanner, result.pid) {
                Self::register_resource_targets(&resource_targets, result.pid, &agent_name);
            }
        }

        // Spawn background threads (config watcher, stale scanner).
        if let Some(ref cfg_path) = config.config_path {
            crate::background::start_config_watcher(
                cfg_path.clone(),
                Arc::clone(&sls_activated),
                Arc::clone(&pending_logtail),
                config.encryption_public_key.clone(),
                config.trace_enabled,
                Arc::clone(&running),
            );
        }
        if let Some(ref sqlite_store) = genai_sqlite_store {
            crate::background::start_stale_scanner(Arc::clone(sqlite_store), Arc::clone(&running));
        }
        let resource_sampler_handle = Self::start_resource_sampler_if_enabled(
            config.features.resource_sampling_enabled,
            genai_sqlite_store.as_ref(),
            Arc::clone(&resource_targets),
            Arc::clone(&running),
        );

        // Trajectory collector (Qoder/QoderWork JSONL → ATIF → trajectories.db).
        // Feature-gated (default off); the thread shares `running` as stop flag.
        if config.features.trajectory_collection_enabled {
            let collector_config = agentsight_trajectory_collector::CollectorConfig {
                scan_interval_secs: config.features.trajectory_scan_interval_secs,
                scan_dirs: config
                    .features
                    .trajectory_scan_dirs
                    .as_ref()
                    .map(|dirs| dirs.iter().map(std::path::PathBuf::from).collect()),
                db_path: config.storage.trajectory_path(),
                maintenance: agentsight_trajectory_collector::TrajectoryMaintenancePolicy {
                    retention_days: config.storage.trajectories.retention_days,
                    max_db_size_mb: config.storage.trajectories.max_db_size_mb,
                },
                maintenance_interval_secs: config.storage.trajectories.check_interval_secs,
            };
            let stop = Arc::clone(&running);
            std::thread::Builder::new()
                .name("trajectory-collector".to_string())
                .spawn(move || {
                    agentsight_trajectory_collector::run_collector_loop(&collector_config, &stop);
                })
                .ok();
        }

        Ok(AgentSight {
            probes,
            parser: Parser::new(),
            aggregator: Aggregator::with_limits(config.connection_capacity, &config.runtime_limits),
            analyzer,
            genai_builder: GenAIBuilder::new(),
            genai_exporters,
            genai_sqlite_store,
            interruption_detector: if config.features.interruption_detection_enabled {
                InterruptionDetector::new(DetectorConfig::default())
            } else {
                InterruptionDetector::disabled()
            },
            interruption_store,
            storage,
            scanner,
            _poller,
            running,
            event_count: 0,
            filewatch_callback: None,
            response_mapper: if config.features.session_mapping_enabled {
                ResponseSessionMapper::with_capacity(config.features.session_mapping_max_entries)
            } else {
                ResponseSessionMapper::disabled()
            },
            pending_genai: Vec::new(),
            // RETRO_FIXUP_CAPACITY is a non-zero constant, so the unwrap is
            // guaranteed unreachable.
            retro_session_fixup: lru::LruCache::new(
                std::num::NonZeroUsize::new(RETRO_FIXUP_CAPACITY).unwrap(),
            ),
            pending_genai_bytes: 0,
            runtime_limits: config.runtime_limits,
            ffi_sender: None,
            last_drain_check: std::time::Instant::now(),
            last_interruption_purge: std::time::Instant::now(),
            last_watermark_log: std::time::Instant::now(),
            pid_agent_name_cache,
            resource_targets,
            resource_sampler_handle,
            http_domains,
            pending_logtail,
            deadloop_kill_enabled: config.deadloop_kill_enabled,
            deadloop_kill_after_count: config.deadloop_kill_after_count,
            process_killer: Arc::new(crate::utils::process::LibcProcessKiller),
            features: config.features.clone(),
            storage_config: config.storage.clone(),
        })
    }

    /// Create storage backend from configuration
    fn create_storage(config: &AgentsightConfig) -> Result<Storage> {
        let sqlite_config = SqliteConfig {
            base_path: config.storage.base_path.clone(),
            db_name: crate::config::PRIMARY_DB_NAME.to_string(),
            audit_table: config.audit_table.clone(),
            token_table: config.token_table.clone(),
            http_table: config.http_table.clone(),
            token_consumption_table: "token_consumption".to_string(),
            retention_days: config.storage.primary.retention_days,
            purge_interval: config.storage.primary.check_interval_inserts,
            max_db_size_mb: config.storage.primary.max_db_size_mb,
        };
        Storage::with_sqlite_config(&sqlite_config)
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Get a clone of the running flag for use in signal handlers
    pub fn running_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }

    /// Get event count
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Attach SSL probes to a specific agent process
    pub fn attach_process(&mut self, pid: u32, agent_name: &str) {
        Self::register_resource_targets(&self.resource_targets, pid, agent_name);
        Self::attach_process_internal(&mut self.probes, pid, agent_name);
    }

    fn register_resource_targets(
        targets: &Arc<RwLock<HashMap<u32, String>>>,
        pid: u32,
        agent_name: &str,
    ) {
        let Ok(mut targets) = targets.write() else {
            log::warn!("Failed to register resource target pid={pid}: target lock poisoned");
            return;
        };
        targets.insert(pid, agent_name.to_string());
        if Self::should_register_descendants(agent_name) {
            for child in Self::collect_descendant_pids(pid) {
                targets.insert(child, agent_name.to_string());
            }
        }
    }

    fn start_resource_sampler_if_enabled(
        enabled: bool,
        store: Option<&Arc<GenAISqliteStore>>,
        targets: Arc<RwLock<HashMap<u32, String>>>,
        running: Arc<AtomicBool>,
    ) -> Option<std::thread::JoinHandle<()>> {
        if !enabled {
            log::debug!("Agent resource sampling disabled");
            return None;
        }
        let Some(store) = store else {
            log::debug!("Resource sampling enabled but SQLite storage is unavailable");
            return None;
        };
        match crate::health::resource::start_resource_sampler(Arc::clone(store), targets, running) {
            Ok(handle) => Some(handle),
            Err(error) => {
                log::warn!("Failed to start Agent resource sampler: {error}");
                None
            }
        }
    }

    /// Resolve the agent name to cache for a connection-scan hit.
    ///
    /// Prefers a cmdline config-rule match; otherwise falls back to the process
    /// comm (`/proc/<pid>/comm`). Returns `None` only when neither is available
    /// (comm unreadable) — such a pid is left uncached for runtime resolution.
    /// Never returns the endpoint domain.
    fn conn_scan_agent_name(scanner: &AgentScanner, pid: u32) -> Option<String> {
        scanner
            .try_match_process(pid)
            .map(|a| a.agent_info.name)
            .or_else(|| crate::discovery::scanner::read_comm(pid))
    }

    /// Internal helper to attach SSL probes to a process
    fn attach_process_internal(probes: &mut Probes, pid: u32, agent_name: &str) {
        if let Err(e) = probes.add_traced_pid(pid) {
            log::warn!("Failed to add pid {pid} to traced_processes map: {e}");
        }
        // Register descendant PIDs so child processes spawned by wrapper-style
        // agents are admitted by the BPF `traced_processes` filter even when
        // procmon misses their execve tracepoint. The SSL uprobe is inode-global
        // (pid=-1), so registering is sufficient — no per-child attach needed.
        //
        // Currently gated to QwenCode: its `/usr/local/bin/qwen` shebang
        // wrapper immediately spawnSync's a `node-22 --expose-gc …cli.js`
        // child, which then spawn's another node-22 worker that does all the
        // actual LLM HTTPS I/O. The wrapper itself makes zero SSL calls, so
        // without the descendants in the BPF map, qwencode's traffic is
        // captured by the uprobe and silently dropped by `is_pid_traced()`.
        // Other agents (CoshNG, Claude, Codex, …) still have their main
        // process participate in TLS, so the matched PID alone is enough.
        if Self::should_register_descendants(agent_name) {
            for child in Self::collect_descendant_pids(pid) {
                if child == pid {
                    continue;
                }
                if let Err(e) = probes.add_traced_pid(child) {
                    log::debug!("Failed to register descendant pid {child} of {pid}: {e}");
                } else {
                    log::debug!("Registered descendant pid {child} of {pid} ({agent_name})");
                }
            }
        }
        // attach_process logs WARN internally if SSL attach degrades to
        // global-uprobe-only coverage; always succeeds for BPF map registration.
        let _ = probes.attach_process(pid as i32);
        log::info!("Attached to agent: {agent_name} (pid={pid})");
    }

    /// Whether descendant PIDs should also be registered into the BPF
    /// `traced_processes` map when an agent is attached.
    ///
    /// Currently true only for `QwenCode` — the only agent whose matched
    /// wrapper process does not itself perform any TLS I/O and whose actual
    /// LLM traffic happens in deeply-nested child `node-22` processes that
    /// procmon tends to miss.
    fn should_register_descendants(agent_name: &str) -> bool {
        matches!(agent_name, "QwenCode")
    }

    /// Collect all descendant PIDs of `root` by walking `/proc`.
    ///
    /// Reads each numeric `/proc/<pid>/status` once to obtain its `PPid`, then
    /// BFS-traverses the parent->children edges. Returns the full set including
    /// `root` itself. Silently skips processes that exit before we read them —
    /// they won't produce SSL events anyway.
    fn collect_descendant_pids(root: u32) -> HashSet<u32> {
        Self::collect_descendant_pids_impl(root, crate::utils::procfs::proc_root())
    }

    fn collect_descendant_pids_impl(root: u32, proc_root: &Path) -> HashSet<u32> {
        let mut result = HashSet::new();
        result.insert(root);

        let proc_dir = match fs::read_dir(proc_root) {
            Ok(d) => d,
            Err(_) => return result,
        };

        let mut children_of: HashMap<u32, Vec<u32>> = HashMap::new();
        for entry in proc_dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let pid: u32 = match name.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let status = match fs::read_to_string(proc_root.join(format!("{pid}/status"))) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let ppid = status
                .lines()
                .find_map(|line| line.strip_prefix("PPid:\t"))
                .and_then(|s| s.trim().parse::<u32>().ok());
            if let Some(ppid) = ppid {
                children_of.entry(ppid).or_default().push(pid);
            }
        }

        let mut queue = vec![root];
        while let Some(p) = queue.pop() {
            if let Some(kids) = children_of.get(&p) {
                for &k in kids {
                    if result.insert(k) {
                        queue.push(k);
                    }
                }
            }
        }
        result
    }

    /// Detach SSL probes from a specific agent process
    pub fn detach_process(&mut self, pid: u32, agent_name: &str) {
        log::debug!("Detaching from pid {pid}, agent name: {agent_name}");
        let _ = self.probes.remove_traced_pid(pid).inspect_err(|e| {
            log::debug!("traced pid {pid} already removed from BPF map (expected race with sched_process_exit): {e}");
        });
        self.probes.detach_ssl_probes(pid);
    }

    /// Add a cgroup inode id to the shared BPF cgroup_filter map at runtime.
    /// Delegates to the underlying `Probes` instance.
    pub fn add_traced_cgroup(&mut self, cgroup_id: u64) -> anyhow::Result<()> {
        self.probes.add_traced_cgroup(cgroup_id)
    }

    /// Remove a cgroup inode id from the shared BPF cgroup_filter map at runtime.
    /// Delegates to the underlying `Probes` instance.
    pub fn remove_traced_cgroup(&mut self, cgroup_id: u64) -> anyhow::Result<()> {
        self.probes.remove_traced_cgroup(cgroup_id)
    }

    /// Try to receive and process the next event (non-blocking)
    /// Returns None if no event is available
    pub fn try_process(&mut self) -> Option<u64> {
        if !self.running.load(Ordering::SeqCst) {
            return None;
        }

        let event = self.probes.try_recv()?;
        self.event_count += 1;

        log::trace!("Processing event: {:?}", event.event_type());

        // Handle ProcMon events for agent lifecycle tracking
        if let Event::ProcMon(ref procmon_event) = event {
            self.handle_procmon_event(procmon_event);
            return None;
        }

        // Handle FileWatch events via callback (not through the pipeline)
        if let Event::FileWatch(ref fw_event) = event {
            self.handle_filewatch_event(fw_event);
            return None;
        }

        // Handle FileWrite events via callback (not through the pipeline)
        if let Event::FileWrite(ref fw_event) = event {
            self.handle_filewrite_event(fw_event);
            // The refreshed pid → session mapping may allow repairing calls
            // that already escaped the deferral window with a fallback id.
            if let Some(ref store) = self.genai_sqlite_store {
                apply_retro_session_fixup(
                    &mut self.retro_session_fixup,
                    &self.response_mapper,
                    store,
                    self.interruption_store.as_deref(),
                    fw_event.pid,
                    self.pid_agent_name_cache
                        .peek(&fw_event.pid)
                        .map(String::as_str),
                );
            }
            // After mapper is updated, try to resolve any pending GenAI events
            self.resolve_pending_genai();
            return None;
        }

        // Handle UDP DNS events (domain-based attachment)
        if let Event::UdpDns(ref dns_event) = event {
            log::debug!(
                "[UDP-DNS] pid={} comm={} domain={}",
                dns_event.pid,
                dns_event.comm,
                dns_event.domain
            );

            // HTTPS rules: attach SSL probes to the process
            if self.scanner.on_dns_event(dns_event.pid, &dns_event.domain) {
                log::info!(
                    "[UDP-DNS] Attaching to pid={} via domain rule (domain={})",
                    dns_event.pid,
                    dns_event.domain
                );
                if let Err(e) = self.probes.attach_process(dns_event.pid as i32) {
                    log::warn!("[UDP-DNS] Failed to attach to pid={}: {}", dns_event.pid, e);
                } else if let Some(agent_name) =
                    Self::conn_scan_agent_name(&self.scanner, dns_event.pid)
                {
                    self.pid_agent_name_cache
                        .put(dns_event.pid, agent_name.clone());
                    if let Ok(mut targets) = self.resource_targets.write() {
                        targets.insert(dns_event.pid, agent_name);
                    }
                }
            }

            // HTTP domains: resolve DNS domain → IP, add to tcpsniff BPF map
            if crate::discovery::matcher::match_domain_glob(&dns_event.domain, &self.http_domains) {
                use std::net::ToSocketAddrs;
                match (dns_event.domain.as_str(), 0u16).to_socket_addrs() {
                    Ok(addrs) => {
                        for addr in addrs {
                            if let std::net::IpAddr::V4(ipv4) = addr.ip() {
                                log::info!(
                                    "[UDP-DNS] Adding http target {} → {}",
                                    dns_event.domain,
                                    ipv4
                                );
                                let target = crate::config::TcpTarget {
                                    ip: Some(ipv4),
                                    port: None,
                                };
                                if let Err(e) = self.probes.add_tcp_target(&target) {
                                    log::warn!("[UDP-DNS] Failed to add tcp target {ipv4}: {e}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "[UDP-DNS] DNS resolve failed for http domain {}: {}",
                            dns_event.domain,
                            e
                        );
                    }
                }
            }

            return None;
        }

        // Parse the event
        let result = self.parser.parse_event(event);

        // Process messages through aggregator
        let aggregated_results = self.aggregator.process_result(result);

        // Analyze and store results
        for agg_result in &aggregated_results {
            let mut analysis_results = self.analyzer.analyze_aggregated(agg_result);

            // Build GenAI semantic events AND pending info in one pass
            let (output, pending_info) = self.genai_builder.build_with_pending(
                &analysis_results,
                &self.response_mapper,
                &self.pid_agent_name_cache,
            );

            // Backfill TokenRecord.agent from pid_agent_name_cache, falling back
            // to the *process* comm (`<procfs root>/<pid>/comm`) and only then
            // the event's per-event thread comm (which may be e.g. "HTTP client").
            for ar in &mut analysis_results {
                if let crate::analyzer::AnalysisResult::Token(t) = ar {
                    if t.agent.is_none() {
                        t.agent = self
                            .pid_agent_name_cache
                            .get(&t.pid)
                            .cloned()
                            .or_else(|| crate::discovery::scanner::read_comm(t.pid))
                            .or_else(|| Some(t.comm.clone()));
                    }
                }
            }

            // FFI fallback: when every built LLM event is semantically empty
            // (the underlying body format could not be parsed into
            // `AgentsightLLMData`), skip the useless LLM event and fall through
            // to raw-HTTP (`AgentsightHttpsData`) reporting instead. Only in FFI
            // mode; requires *all* events to be empty LLM calls so we never drop
            // a meaningful event.
            let ffi_https_fallback =
                self.ffi_sender.is_some() && events_are_empty_llm(&output.events);

            if !output.events.is_empty() && !ffi_https_fallback {
                if let Some(response_id) = output.pending_response_id {
                    // Session_id not yet resolved — queue for deferred resolution.
                    // Write a pending row NOW so crash detection can see this call
                    // during the deferral window (up to PENDING_SESSION_TIMEOUT).
                    if let Some(ref info) = pending_info {
                        if let Some(sqlite_store) = self.genai_sqlite_store.as_ref() {
                            if let Err(e) = sqlite_store.insert_pending(info) {
                                log::warn!(
                                    "Failed to insert deferred pending call {}: {}",
                                    info.call_id,
                                    e
                                );
                            }
                        }
                    } else {
                        log::warn!(
                            "Deferred GenAI call queued without pending_info (response_id={}), crash detection blind spot remains",
                            response_id
                        );
                    }
                    self.pending_genai.push(PendingGenAI {
                        events: output.events,
                        response_id,
                        pid: pending_info.as_ref().map(|p| p.pid as u32).unwrap_or(0),
                        created_at: std::time::Instant::now(),
                    });
                    self.pending_genai_bytes +=
                        self.pending_genai.last().map_or(0, |p| p.estimated_bytes());
                    self.enforce_pending_genai_limits();
                    log::debug!("GenAI events queued for deferred session_id resolution");
                } else {
                    // Session_id resolved (or no response_id) — export immediately.
                    // For SQLite: write pending first, then complete_pending;
                    // for other exporters: normal export.
                    if let Some(ref info) = pending_info {
                        if let Some(sqlite_store) = self.genai_sqlite_store.as_ref() {
                            if let Err(e) = sqlite_store.insert_pending(info) {
                                log::warn!("Failed to insert pending call {}: {}", info.call_id, e);
                            }
                            for event in &output.events {
                                if let Err(e) = sqlite_store.complete_pending(event) {
                                    log::warn!("Failed to complete pending call: {e}");
                                }
                            }
                            // Export to non-SQLite exporters only (SQLite already written)
                            for exporter in &self.genai_exporters {
                                if exporter.name() != "sqlite" {
                                    exporter.export(&output.events);
                                    log::debug!(
                                        "Exported {} GenAI events via '{}'",
                                        output.events.len(),
                                        exporter.name()
                                    );
                                }
                            }
                            if let Some(ref sender) = self.ffi_sender {
                                for event in &output.events {
                                    if let GenAISemanticEvent::LLMCall(call) = event {
                                        sender.send_llm(call);
                                    }
                                }
                            }
                        } else {
                            self.export_genai_events(&output.events);
                        }
                    } else {
                        self.export_genai_events(&output.events);
                    }

                    // ── Online interruption detection ─────────────────────────────
                    // Run after export so the call is already persisted.
                    self.detect_and_store_interruptions(&output.events);
                }
            } else if let Some(ref sender) = self.ffi_sender {
                // Either no LLM event was produced, or all LLM events were
                // semantically empty (fallback). The FFI sender suppresses
                // raw HTTPS before cloning or enqueueing when it is disabled.
                for ar in &analysis_results {
                    if let crate::analyzer::AnalysisResult::Http(record) = ar {
                        // Resolved here rather than in the FFI layer because the cache
                        // lives on `self` and outlives the process, so an exited agent
                        // still resolves. Same ladder as the LLM path in
                        // `GenAICallBuilder::build` so both report the same value.
                        //
                        // Passed as a closure so `send_https` can skip it entirely when
                        // raw HTTPS is disabled — the default — instead of paying two
                        // `/proc` reads per non-LLM exchange for a value it drops.
                        sender.send_https(record, || {
                            Some(
                                GenAIBuilder::resolve_agent_name(
                                    &record.comm,
                                    record.pid,
                                    &self.pid_agent_name_cache,
                                )
                                .unwrap_or_else(|| record.comm.clone()),
                            )
                        });
                    }
                }
            }

            // In FFI mode data is delivered via callbacks; skip local storage.
            if self.ffi_sender.is_none() {
                for analysis_result in &analysis_results {
                    if !self.should_persist_analysis_result(analysis_result) {
                        continue;
                    }
                    if let Err(e) = self.storage.store(analysis_result) {
                        log::warn!("Failed to store analysis result: {e}");
                    } else {
                        log::debug!("Analysis result saved");
                    }
                }
            }
        }

        Some(self.event_count)
    }

    /// Handle ProcMon event for agent lifecycle tracking
    fn handle_procmon_event(&mut self, event: &crate::probes::procmon::Event) {
        use crate::probes::procmon::Event as ProcMonEvent;

        match event {
            ProcMonEvent::Exec { pid, comm, .. } => {
                // Read cmdline for deny-check and custom matching
                let cmdline_args = crate::discovery::scanner::read_cmdline(pid);

                // Phase 1: check deny rules first (blacklist overrides everything)
                if self.scanner.is_denied(&cmdline_args) {
                    log::debug!("ProcMon: pid={pid} denied by cmdline rule, skipping attach");
                    return;
                }

                // Phase 2: check if this is a known agent and start tracking
                if let Some(agent) = self.scanner.on_process_create(*pid, comm) {
                    let agent_name = agent.agent_info.name.clone();
                    self.pid_agent_name_cache.put(*pid, agent_name.clone());
                    self.attach_process(*pid, &agent_name);
                }
            }
            ProcMonEvent::Exit { pid, exit_code, .. } => {
                if let Ok(mut targets) = self.resource_targets.write() {
                    targets.remove(pid);
                }
                // Remove from tracking if it was an agent
                if let Some(agent) = self.scanner.on_process_exit(*pid) {
                    let agent_name = agent.agent_info.name.clone();
                    self.probes.detach_ssl_probes(*pid);
                    self.handle_agent_crash_detection(*pid, &agent_name, *exit_code);
                }
            }
        }
    }

    /// Handle FileWatch event via registered callback
    fn handle_filewatch_event(&self, event: &FileWatchEvent) {
        log::debug!("FileWatch: pid={} file={}", event.pid, event.filename);
        if let Some(ref cb) = self.filewatch_callback {
            cb(event.clone());
        }
    }

    /// Register a callback for file watch events (.jsonl file opens)
    pub fn on_filewatch<F>(&mut self, callback: F)
    where
        F: Fn(FileWatchEvent) + Send + 'static,
    {
        self.filewatch_callback = Some(Box::new(callback));
    }

    /// Handle FileWrite event: extract responseId→sessionId mapping, then call callback
    fn handle_filewrite_event(&mut self, event: &FileWriteEvent) {
        log::debug!(
            "FileWrite: pid={} file={} size={}",
            event.pid,
            event.filename,
            event.write_size
        );
        self.response_mapper.process_filewrite(event);
    }

    /// Run the event loop (blocking)
    pub fn run(&mut self) -> Result<u64> {
        log::debug!("Agent discovery running via ProcMon events");

        // Main event loop
        while self.running.load(Ordering::SeqCst) {
            // Rate-limited internally, and deliberately outside the idle branch:
            // a saturated byte budget keeps the queue non-empty, which is exactly
            // when the watermarks matter most.
            self.maybe_log_buffer_watermarks();
            if let Some(result) = self.try_process() {
                log::trace!("[Event {result}] Processed");
            } else {
                // No event available — flush any timed-out pending GenAI events
                self.flush_expired_pending_genai();
                // Drain orphaned connections from dead PIDs and persist as pending
                self.drain_and_persist_dead_connections();
                // Snapshot idle in-flight streams whose owning process is still alive.
                self.drain_and_persist_idle_connections();
                // Check if config watcher deposited a new LogtailExporter
                self.check_pending_logtail();
                // Periodically purge old/oversized interruption DB entries
                self.maybe_purge_interruption_store();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        // On shutdown, flush all remaining pending events with fallback session_id
        self.flush_all_pending_genai();

        Ok(self.event_count)
    }

    /// Shutdown gracefully
    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.resource_sampler_handle.take()
            && handle.join().is_err()
        {
            log::warn!("Agent resource sampler panicked during shutdown");
        }
        // Flush all pending GenAI events before exit
        self.flush_all_pending_genai();
        // Checkpoint genai_events.db WAL so -wal/-shm are cleaned up on exit
        // (mirrors Storage::Drop which checkpoints agentsight.db).
        if let Some(ref store) = self.genai_sqlite_store {
            if let Err(e) = store.wal_checkpoint() {
                log::warn!("GenAI WAL checkpoint on shutdown failed: {e}");
            }
        }
    }

    /// Check and drain the pending_logtail mailbox.
    /// If the config watcher deposited a new LogtailExporter, register it.
    fn check_pending_logtail(&mut self) {
        if let Ok(mut guard) = self.pending_logtail.try_lock() {
            if let Some(exporter) = guard.take() {
                log::info!(
                    "Registering dynamically-activated LogtailExporter: '{}'",
                    exporter.name()
                );
                self.genai_exporters.push(exporter);
            }
        }
    }

    /// Install an FFI event sender for C API mode.
    /// When set, completed events are pushed through this channel.
    /// `pub(crate)` because `FfiEventSender` is a crate-internal type and the
    /// only caller lives in this crate's FFI layer.
    pub(crate) fn set_ffi_sender(&mut self, sender: FfiEventSender) {
        self.ffi_sender = Some(sender);
    }

    /// Export GenAI events to all registered exporters
    fn export_genai_events(&self, events: &[GenAISemanticEvent]) {
        if let Some(ref sender) = self.ffi_sender {
            // FFI mode: deliver LLMCall events via callback channel only.
            for event in events {
                if let GenAISemanticEvent::LLMCall(call) = event {
                    sender.send_llm(call);
                }
            }
        } else {
            // Normal mode: export to all registered exporters.
            for exporter in &self.genai_exporters {
                exporter.export(events);
                log::debug!(
                    "Exported {} GenAI events via '{}'",
                    events.len(),
                    exporter.name()
                );
            }
        }
    }

    /// Complete deferred GenAI events: promote their pending DB rows to
    /// 'complete', then export to non-SQLite exporters (or FFI).
    ///
    /// # Preconditions
    ///
    /// A `status='pending'` row for each event's `call_id` must already exist
    /// in `genai_events` (written by `insert_pending` at queue time).
    ///
    /// This mirrors the immediate path (try_process lines 717-744) but is used
    /// when events were queued in `pending_genai` and are now being drained.
    /// The pending row was written by the deferred-queue entry point; this
    /// method updates it via `complete_pending` and avoids double-writing by
    /// skipping the SQLite exporter in the fan-out.
    fn complete_and_export_deferred_genai(&self, events: &[GenAISemanticEvent]) {
        complete_deferred_genai(
            events,
            self.genai_sqlite_store.as_ref(),
            &self.genai_exporters,
            self.ffi_sender.as_ref(),
        );
    }

    /// Online interruption detection: inspect exported events and persist any
    /// detected interruption records.  Also stamps the `interruption_type`
    /// column on the corresponding `genai_events` row when SQLite is in use.
    fn detect_and_store_interruptions(&self, events: &[GenAISemanticEvent]) {
        if let Some(ref istore) = self.interruption_store {
            // Build a call_id → (session_id, conversation_id) lookup from
            // LLMCall events in this batch, so ToolUse events can inherit
            // the conversation context of their parent call.
            let call_context: std::collections::HashMap<String, (Option<String>, Option<String>)> =
                events
                    .iter()
                    .filter_map(|e| {
                        if let GenAISemanticEvent::LLMCall(c) = e {
                            Some((
                                c.call_id.clone(),
                                (
                                    c.metadata.get("session_id").cloned(),
                                    c.metadata.get("conversation_id").cloned(),
                                ),
                            ))
                        } else {
                            None
                        }
                    })
                    .collect();

            for event in events {
                if let GenAISemanticEvent::LLMCall(llm_call) = event {
                    let interruptions = self.interruption_detector.detect(llm_call);
                    for ie in &interruptions {
                        // Deduplicate: skip if same (conversation_id, type, error_msg)
                        // already recorded.  Same error retried N times produces only
                        // 1 interruption; different errors each get 1.
                        // NOTE: RetryStorm detection only fires when conversation_id is Some.
                        // When None, each error inserts a separate row (no dedup, no storm detect).
                        if let Some(ref cid) = ie.conversation_id {
                            let error_msg = llm_call.error.as_deref();
                            if istore.exists_for_conversation(cid, &ie.interruption_type, error_msg)
                            {
                                log::debug!(
                                    "Skipping duplicate {:?} for conversation_id={} error={:?}",
                                    ie.interruption_type,
                                    cid,
                                    error_msg
                                );
                                // Still stamp the genai_events row so the call is marked
                                if let Some(ref sqlite) = self.genai_sqlite_store {
                                    let _ = sqlite.update_interruption_type(
                                        &llm_call.call_id,
                                        ie.interruption_type.as_str(),
                                    );
                                    // RetryStorm: if >= 5 total calls with same error type in
                                    // this conversation, emit critical alert
                                    let count = sqlite.count_interruption_type_for_conversation(
                                        cid,
                                        ie.interruption_type.as_str(),
                                    );
                                    if count >= 5
                                        && ie.interruption_type
                                            != crate::interruption::InterruptionType::RetryStorm
                                    {
                                        let storm_event =
                                            crate::interruption::InterruptionEvent::new(
                                                crate::interruption::InterruptionType::RetryStorm,
                                                ie.session_id.clone(),
                                                ie.trace_id.clone(),
                                                ie.conversation_id.clone(),
                                                ie.call_id.clone(),
                                                ie.pid,
                                                ie.agent_name.clone(),
                                                llm_call.end_timestamp_ns as i64,
                                                Some(serde_json::json!({
                                                    "repeated_type": ie.interruption_type.as_str(),
                                                    "count": count,
                                                })),
                                            );
                                        if !istore.exists_for_conversation(
                                            cid,
                                            &crate::interruption::InterruptionType::RetryStorm,
                                            None,
                                        ) {
                                            let _ = istore.insert(&storm_event);
                                            log::warn!(
                                                "RetryStorm detected: {} × {:?} in conversation {}",
                                                count,
                                                ie.interruption_type,
                                                cid
                                            );
                                        }
                                    }
                                }
                                continue;
                            }
                        }
                        if let Err(e) = istore.insert(ie) {
                            log::warn!("Failed to store interruption event: {e}");
                        }
                        // Also export to iLogtail file (no-op if SLS_LOGTAIL_FILE unset),
                        // so the SLS index keeps interruption records co-located with LLM calls.
                        crate::genai::logtail::export_interruption_events(std::slice::from_ref(ie));
                        // Also stamp genai_events row with interruption_type
                        if let Some(ref sqlite) = self.genai_sqlite_store {
                            let _ = sqlite.update_interruption_type(
                                &llm_call.call_id,
                                ie.interruption_type.as_str(),
                            );
                        }
                    }

                    // ── Cross-call DeadLoop detection ──────────────────────────────
                    // After single-call detection, check for repetitive patterns
                    // across the conversation's recent calls.
                    if let Some(ref cid) = llm_call.metadata.get("conversation_id") {
                        // When auto-kill is enabled, allow multiple detection events
                        // (up to kill_after_count) so the count threshold can be reached.
                        // When disabled, deduplicate to at most one event per conversation.
                        let existing_count = istore.count_for_conversation(
                            cid,
                            &crate::interruption::InterruptionType::DeadLoop,
                        );
                        let should_detect = if self.deadloop_kill_enabled {
                            existing_count <= self.deadloop_kill_after_count
                        } else {
                            existing_count == 0
                        };

                        if should_detect {
                            if let Some(ref sqlite) = self.genai_sqlite_store {
                                let loop_detector = crate::interruption::LoopDetector::default();
                                let recent = sqlite.get_recent_calls_for_conversation(
                                    cid,
                                    loop_detector.config.window_size,
                                );
                                if let Some(loop_event) = loop_detector.detect(
                                    cid,
                                    llm_call.metadata.get("session_id").map(|s| s.as_str()),
                                    llm_call.agent_name.as_deref(),
                                    Some(llm_call.pid),
                                    llm_call.end_timestamp_ns as i64,
                                    &recent,
                                ) {
                                    let _ = istore.insert(&loop_event);
                                    crate::genai::logtail::export_interruption_events(
                                        std::slice::from_ref(&loop_event),
                                    );
                                    log::warn!(
                                        "DeadLoop detected in conversation {}: {:?}",
                                        cid,
                                        loop_event.detail
                                    );

                                    // ── Auto-kill 止血 ──
                                    let new_count = existing_count + 1;
                                    let action = crate::interruption::escalation::decide_kill(
                                        self.deadloop_kill_enabled,
                                        existing_count,
                                        self.deadloop_kill_after_count,
                                    );
                                    crate::interruption::escalation::execute_kill_action(
                                        self.process_killer.as_ref(),
                                        action,
                                        loop_event.pid,
                                        cid,
                                        new_count,
                                        self.deadloop_kill_after_count,
                                    );
                                }
                            }
                        }
                    }
                } else if let GenAISemanticEvent::ToolUse(tool) = event {
                    // ── Tool failure detection ──────────────────────────────
                    let (session_id, conversation_id) = tool
                        .parent_llm_call_id
                        .as_ref()
                        .and_then(|cid| call_context.get(cid))
                        .cloned()
                        .unwrap_or((None, None));
                    let interruptions = self.interruption_detector.detect_tool_use(
                        tool,
                        session_id,
                        conversation_id,
                    );
                    for ie in &interruptions {
                        // Deduplicate against unresolved interruptions already recorded
                        // for this conversation, mirroring the LLMCall path above: a
                        // tool that keeps failing the same way in a loop should not
                        // flood the store with one row per attempt.
                        if let Some(ref cid) = ie.conversation_id {
                            let error_msg = tool.error.as_deref();
                            if istore.exists_for_conversation(cid, &ie.interruption_type, error_msg)
                            {
                                log::debug!(
                                    "Skipping duplicate {:?} for conversation_id={} tool={}",
                                    ie.interruption_type,
                                    cid,
                                    tool.tool_name
                                );
                                continue;
                            }
                        }
                        if let Err(e) = istore.insert(ie) {
                            log::warn!("Failed to store tool_failure interruption: {e}");
                        }
                        crate::genai::logtail::export_interruption_events(std::slice::from_ref(ie));
                        log::warn!(
                            "ToolFailure detected: tool={} error={:?}",
                            tool.tool_name,
                            tool.error
                        );
                    }
                }
            }
        }
    }

    /// Immediate crash detection when a tracked agent process exits.
    ///
    /// Called from `ProcMon::Exit` handler. Flushes the pid's deferred GenAI
    /// events (issue #2032), drains in-flight connections for the PID and
    /// persists them as pending calls regardless of how the process
    /// terminated, then delegates the crash decision to
    /// [`record_agent_crash_interruptions`], passing the decoded raw
    /// `task_struct->exit_code` so clean exits are not misreported.
    fn handle_agent_crash_detection(&mut self, pid: u32, agent_name: &str, raw_exit_code: u32) {
        // Flush this pid's deferred GenAI events first: the exited process can
        // never produce the awaited session_id mapping, and the pending-calls
        // query below must see those already-answered calls as 'complete' so
        // neither the grader (issue #2032) nor the crash path below treats
        // them as unanswered.
        self.flush_deferred_genai_for_pid(pid);

        // Persist the raw exit status to the shared interruption DB first: the
        // serve-mode HealthChecker runs in a separate process and re-detects
        // this exit as "offline + pending" on its next cycle. Without this
        // record it would re-report a clean exit as agent_crash, undoing the
        // issue #1989 fix on the backup path.
        if let Some(ref istore) = self.interruption_store {
            if let Err(e) = istore.record_process_exit(pid as i32, raw_exit_code) {
                log::warn!("[CrashDetect] Failed to record exit status for pid={pid}: {e}");
                // Best-effort: drop any stale row from a previous process with
                // this pid so serve mode cannot misread an old clean exit as
                // this exit within the lookup window. If the DB is broken the
                // delete likely fails too — serve then merely degrades to the
                // pre-fix behavior (offline + pending → crash), never worse.
                if let Err(e) = istore.clear_process_exit(pid as i32) {
                    log::warn!(
                        "[CrashDetect] Failed to clear stale exit status for pid={pid}: {e}"
                    );
                }
            }
        }

        // 1. Drain in-flight connections for this PID from the aggregator
        let drained = self.aggregator.drain_connections_for_pid(pid);

        // 2. Persist drained connections as pending calls
        for (conn_id, state) in &drained {
            let Some(request) = state.pending_request() else {
                continue;
            };

            if let Some(pending) = self.genai_builder.build_pending_from_request(
                request,
                conn_id,
                &self.response_mapper,
                &self.pid_agent_name_cache,
            ) {
                if let Some(ref store) = self.genai_sqlite_store {
                    if let Err(e) = store.insert_pending(&pending) {
                        log::warn!("[CrashDetect] Failed to persist pending call: {e}");
                    }
                }
            }
        }

        // 3. Query all pending calls for this PID (including any persisted earlier)
        let pending_calls = if let Some(ref store) = self.genai_sqlite_store {
            store
                .list_pending_for_pids(&[pid as i32])
                .unwrap_or_default()
        } else {
            vec![]
        };

        if pending_calls.is_empty() {
            log::debug!(
                "[CrashDetect] Agent {agent_name} (pid={pid}) exited with no pending calls — normal shutdown",
            );
            return;
        }

        // 4. Record agent_crash interruptions unless the exit was clean
        if let Some(ref istore) = self.interruption_store {
            record_agent_crash_interruptions(
                pid,
                agent_name,
                ProcessExitStatus::decode(raw_exit_code),
                &pending_calls,
                istore,
                self.genai_sqlite_store.as_deref(),
            );
        }
    }

    /// Snapshot idle in-flight streams and persist them as `pending` records.
    ///
    /// This covers manual output interruption where the agent process stays
    /// alive, so dead-PID draining cannot discover the abandoned stream.
    fn drain_and_persist_idle_connections(&mut self) {
        let snapshots = self.aggregator.snapshot_idle_connections();
        if snapshots.is_empty() {
            return;
        }

        use crate::storage::sqlite::PendingOrigin;

        for (conn_id, state) in snapshots {
            let Some(request) = state.pending_request() else {
                continue;
            };

            if let Some(mut pending) = self.genai_builder.build_pending_from_request(
                request,
                &conn_id,
                &self.response_mapper,
                &self.pid_agent_name_cache,
            ) {
                pending.pending_origin = PendingOrigin::IdleDrain;
                if let Some(ref store) = self.genai_sqlite_store {
                    let call_id = pending.call_id.clone();
                    if let Err(e) = store.insert_pending(&pending) {
                        log::warn!("[IdleDrain] Failed to persist pending call: {e}");
                        continue;
                    }
                    log::info!(
                        "[IdleDrain] persisted idle in-flight call as pending: call_id={} pid={} path={}",
                        call_id,
                        conn_id.pid,
                        request.path,
                    );
                }
            } else {
                log::debug!(
                    "[IdleDrain] build_pending returned None: pid={} path={} body_len={}",
                    conn_id.pid,
                    request.path,
                    request.body_len
                );
            }
        }
    }

    /// Drain aggregator connections whose PID is no longer alive and persist
    /// them as `pending` records in `genai_events`.  Rate-limited to once per
    /// second to avoid excessive `/proc` scanning.
    fn drain_and_persist_dead_connections(&mut self) {
        if self.last_drain_check.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.last_drain_check = std::time::Instant::now();

        let drained = self.aggregator.drain_dead_pid_connections();
        if drained.is_empty() {
            return;
        }

        use crate::aggregator::ConnectionState;
        use crate::genai::GenAIBuilder;

        // Track persisted pending calls: (pid, call_id, session_id, agent_name, conversation_id)
        let mut persisted_pending: Vec<(
            u32,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = Vec::new();

        for (conn_id, state) in drained {
            let Some(request) = state.pending_request().cloned() else {
                continue;
            };
            // Preserve SSE enrichment while sharing request selection with other drains.
            // For compressed SSE streams that were still in progress when the
            // process died (sse_events empty, compressed_buffer non-empty),
            // decode the buffer here so token-usage data is not lost (#973).
            let sse_events = match state {
                ConnectionState::SseActive {
                    sse_events,
                    compressed_buffer: Some(buf),
                    content_encoding,
                    response_headers,
                    zstd_decoder,
                    ..
                } if sse_events.is_empty() && !buf.is_empty() => {
                    // fix(#973): decode the unfinalized compressed buffer
                    // so drain-path token extraction can proceed.
                    let is_chunked =
                        crate::aggregator::HttpConnectionAggregator::is_chunked_response(
                            &response_headers,
                        );
                    crate::aggregator::HttpConnectionAggregator::decode_compressed_sse(
                        &buf,
                        content_encoding.as_deref(),
                        is_chunked,
                        zstd_decoder.as_deref(),
                        &response_headers.source_event,
                    )
                }
                ConnectionState::SseActive { sse_events, .. } => sse_events,
                _ => vec![],
            };

            if let Some(pending) = self.genai_builder.build_pending_from_request(
                &request,
                &conn_id,
                &self.response_mapper,
                &self.pid_agent_name_cache,
            ) {
                if let Some(ref store) = self.genai_sqlite_store {
                    let call_id = pending.call_id.clone();
                    let pid = pending.pid;

                    if let Err(e) = store.insert_pending(&pending) {
                        log::warn!("[DrainCheck] FAIL persist: {e}");
                        continue;
                    }
                    // Track for OOM detection below
                    persisted_pending.push((
                        conn_id.pid,
                        pending.call_id.clone(),
                        pending.session_id.clone(),
                        pending.agent_name.clone(),
                        pending.conversation_id.clone(),
                    ));
                    // ── Session ID reconciliation ──────────────────────────
                    // The drain path computes session_id via the response_id
                    // domain-separated hash fallback (`SHA256("session"+rid)`),
                    // but normal flow uses ResponseSessionMapper (agent .jsonl UUID).
                    // Look up the real session_id from completed records for the same PID.
                    match store.lookup_session_for_pid(pid) {
                        Ok(Some(ref real_session_id)) => {
                            if pending.session_id.as_deref() != Some(real_session_id.as_str()) {
                                if let Err(e) = store.update_session_id(&call_id, real_session_id) {
                                    log::warn!("[DrainCheck] FAIL update session_id: {e}");
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            log::warn!("[DrainCheck] FAIL lookup session: {e}");
                        }
                    }

                    // ── SSE enrichment ────────────────────────────────────
                    // Parse captured SSE events for model, trace_id, tokens, output content
                    if !sse_events.is_empty() {
                        if let Some(mut enrichment) =
                            GenAIBuilder::extract_sse_enrichment(&sse_events)
                        {
                            // If SSE didn't carry usage data (stream was interrupted before
                            // the final chunk), compute tokens via the real tokenizer.
                            if enrichment.input_tokens.is_none()
                                || enrichment.output_tokens.is_none()
                            {
                                let model_name = enrichment
                                    .model
                                    .as_deref()
                                    .or(pending.model.as_deref())
                                    .unwrap_or("unknown");
                                if let Ok(tokenizer) =
                                    crate::tokenizer::get_global_tokenizer(model_name)
                                {
                                    // ── input tokens ──
                                    if enrichment.input_tokens.is_none() {
                                        if let Some(body) = request.json_body() {
                                            if let Some(messages) =
                                                body.get("messages").and_then(|m| m.as_array())
                                            {
                                                let mut msgs = messages.clone();
                                                // Parse tool_calls.arguments from string to object
                                                for msg in msgs.iter_mut() {
                                                    if let Some(tcs) = msg
                                                        .get_mut("tool_calls")
                                                        .and_then(|tc| tc.as_array_mut())
                                                    {
                                                        for tc in tcs.iter_mut() {
                                                            if let Some(f) = tc.get_mut("function")
                                                            {
                                                                if let Some(a) = f
                                                                    .get("arguments")
                                                                    .and_then(|a| a.as_str())
                                                                {
                                                                    if let Ok(p) =
                                                                        serde_json::from_str::<
                                                                            serde_json::Value,
                                                                        >(
                                                                            a
                                                                        )
                                                                    {
                                                                        f["arguments"] = p;
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                let tools_json: Option<Vec<serde_json::Value>> =
                                                    body.get("tools")
                                                        .and_then(|t| t.as_array())
                                                        .map(|a| a.to_vec());
                                                let count = match tokenizer
                                                    .apply_chat_template_with_tools(
                                                        &msgs,
                                                        tools_json.as_deref(),
                                                        true,
                                                    ) {
                                                    Ok(formatted) => {
                                                        tokenizer.count(&formatted).unwrap_or(0)
                                                    }
                                                    Err(_) => {
                                                        // Fallback: raw message count
                                                        msgs.iter()
                                                            .filter_map(|m| {
                                                                serde_json::to_string(m).ok()
                                                            })
                                                            .map(|s| {
                                                                tokenizer.count(&s).unwrap_or(0)
                                                            })
                                                            .sum()
                                                    }
                                                };
                                                if count > 0 {
                                                    enrichment.input_tokens = Some(count as i64);
                                                }
                                            }
                                        }
                                    }
                                    // ── output tokens ──
                                    if enrichment.output_tokens.is_none() {
                                        use crate::analyzer::token::extract_response_content;
                                        let mut all_content = String::new();
                                        let mut all_reasoning = String::new();
                                        let mut all_tool_calls = Vec::new();
                                        for ev in &sse_events {
                                            if let Some(chunk) = ev.json_body() {
                                                if let Some((content, reasoning, tool_calls)) =
                                                    extract_response_content(Some(&chunk))
                                                {
                                                    if !content.is_empty() {
                                                        all_content.push_str(&content);
                                                    }
                                                    if let Some(r) = reasoning {
                                                        if !r.is_empty() {
                                                            all_reasoning.push_str(&r);
                                                        }
                                                    }
                                                    for tc in tool_calls {
                                                        if !tc.is_empty() {
                                                            all_tool_calls.push(tc);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        let mut total = 0usize;
                                        if !all_reasoning.is_empty() {
                                            let wrapped =
                                                format!("<think>\n{all_reasoning}\n</think>\n\n");
                                            total += tokenizer.count(&wrapped).unwrap_or(0);
                                        }
                                        if !all_content.is_empty() {
                                            total += tokenizer.count(&all_content).unwrap_or(0);
                                        }
                                        if !all_tool_calls.is_empty() {
                                            total += tokenizer
                                                .count(&all_tool_calls.join(""))
                                                .unwrap_or(0);
                                        }
                                        if total > 0 {
                                            enrichment.output_tokens = Some(total as i64);
                                        }
                                    }
                                } else {
                                    log::warn!(
                                        "[DrainCheck] tokenizer unavailable for model {:?}, skipping token computation",
                                        enrichment.model.as_deref().or(pending.model.as_deref())
                                    );
                                }
                            }
                            if let Err(e) = store.enrich_pending_from_sse(&call_id, &enrichment) {
                                log::warn!("[DrainCheck] FAIL enrich SSE: {e}");
                            }
                        }
                    }
                }
            } else {
                log::debug!(
                    "[DrainCheck] build_pending returned None: pid={} path={} body_len={}",
                    conn_id.pid,
                    request.path,
                    request.body_len
                );
            }
        }

        // ── OOM detection for dead PIDs ──────────────────────────────────────
        // After persisting pending calls for dead PIDs, check if any were OOM-killed.
        // This runs in the trace process (every 1s) and catches OOM events much faster
        // than the HealthChecker (30s cycle in serve process).
        if !persisted_pending.is_empty() {
            if let Some(ref istore) = self.interruption_store {
                use crate::interruption::{
                    InterruptionEvent, InterruptionType, was_pid_oom_killed,
                };

                let now_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);

                let mut checked_pids: HashSet<u32> = HashSet::new();
                for (pid, _call_id, session_id, agent_name, conversation_id) in &persisted_pending {
                    if !checked_pids.insert(*pid) {
                        continue; // already checked this PID
                    }
                    if was_pid_oom_killed(*pid as i32) {
                        let call_ids: Vec<&str> = persisted_pending
                            .iter()
                            .filter(|(p, _, _, _, _)| *p == *pid)
                            .map(|(_, c, _, _, _)| c.as_str())
                            .collect();
                        log::info!(
                            "[DrainCheck] PID {} was OOM-killed (confirmed via dmesg), agent={}, calls={:?}",
                            pid,
                            agent_name.as_deref().unwrap_or("unknown"),
                            call_ids
                        );
                        let detail = serde_json::json!({
                            "pid": pid,
                            "agent_name": agent_name,
                            "call_ids": call_ids,
                            "oom": true,
                            "source": "drain+dmesg",
                        });
                        let event = InterruptionEvent::new(
                            InterruptionType::AgentCrash,
                            session_id.clone(),
                            None,
                            conversation_id.clone(),
                            None,
                            Some(*pid as i32),
                            agent_name.clone(),
                            now_ns,
                            Some(detail),
                        );
                        if let Err(e) = istore.insert(&event) {
                            log::warn!(
                                "[DrainCheck] Failed to record OOM agent_crash for pid={pid}: {e}"
                            );
                        } else {
                            log::info!("[DrainCheck] Recorded OOM agent_crash for pid={pid}");
                        }
                        // Mark all pending calls for this PID as interrupted
                        if let Some(ref store) = self.genai_sqlite_store {
                            if let Err(e) =
                                store.mark_pending_interrupted_for_pid(*pid as i32, "oom_crash")
                            {
                                log::warn!(
                                    "[DrainCheck] Failed to mark pending interrupted for pid={pid}: {e}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Remember timeout-escaped LLM calls so a later FileWrite from the same
    /// pid can retroactively repair their fallback session_id (issue #2059).
    fn register_retro_session_fixup(&mut self, pid: u32, events: &[GenAISemanticEvent]) {
        let call_ids = retro_fixup_call_ids(pid, events);
        if call_ids.is_empty() {
            return;
        }
        // All events of a deferred batch belong to one call from one process,
        // so the first LLMCall's agent_name identifies the pid's owner. It is
        // compared against the pid's owner at fix-up time to catch pid reuse.
        let agent_name = events.iter().find_map(|e| match e {
            GenAISemanticEvent::LLMCall(call) => Some(call.agent_name.clone()),
            _ => None,
        });
        let agent_name = agent_name.flatten();
        let now = std::time::Instant::now();
        let mut entries = self.retro_session_fixup.pop(&pid).unwrap_or_default();
        // Drop already-expired entries so a chatty pid cannot grow the list
        // beyond what the fix-up window can ever consume.
        entries.retain(|(_, ts, _)| ts.elapsed() <= RETRO_FIXUP_WINDOW);
        entries.extend(call_ids.into_iter().map(|id| (id, now, agent_name.clone())));
        // `push` (unlike `put`) reports the LRU victim; the pid's own entry
        // was popped above, so any Some here is a capacity eviction — surface
        // it because the evicted pid silently loses its fix-up chance.
        if let Some((evicted_pid, evicted)) = self.retro_session_fixup.push(pid, entries) {
            if evicted_pid != pid {
                log::debug!(
                    "retro fix-up registry full: evicted pid={evicted_pid} with {} pending fix-up(s)",
                    evicted.len()
                );
            }
        }
    }

    /// Try to resolve pending GenAI events whose session_id can now be looked up.
    /// Called after FileWrite events update the ResponseSessionMapper.
    fn resolve_pending_genai(&mut self) {
        if self.pending_genai.is_empty() {
            return;
        }

        let pending_items: Vec<_> = self.pending_genai.drain(..).collect();
        let mut still_pending = Vec::new();
        let mut to_export: Vec<Vec<GenAISemanticEvent>> = Vec::new();

        for mut pending in pending_items {
            if let Some(session_id) = self
                .response_mapper
                .get_session_by_response_id(&pending.response_id)
                .or_else(|| self.response_mapper.get_session_by_pid(pending.pid))
                .map(|s| s.to_string())
            {
                // Resolved — update session_id in all event metadata
                log::debug!(
                    "Deferred session_id resolved: response_id={} → session_id={}",
                    pending.response_id,
                    session_id
                );
                for event in &mut pending.events {
                    if let GenAISemanticEvent::LLMCall(call) = event {
                        call.metadata
                            .insert("session_id".to_string(), session_id.clone());
                    }
                }
                to_export.push(pending.events);
            } else if pending.created_at.elapsed() >= PENDING_SESSION_TIMEOUT {
                // Timed out — export with fallback session_id
                log::debug!(
                    "Deferred session_id timed out for response_id={}, using fallback",
                    pending.response_id
                );
                self.register_retro_session_fixup(pending.pid, &pending.events);
                to_export.push(pending.events);
            } else {
                // Still waiting
                still_pending.push(pending);
            }
        }

        self.pending_genai = still_pending;

        for events in &to_export {
            self.complete_and_export_deferred_genai(events);
            self.detect_and_store_interruptions(events);
        }
    }

    /// Flush any pending GenAI events that have exceeded the timeout.
    /// Called during idle periods of the event loop.
    pub fn flush_expired_pending_genai(&mut self) {
        if self.pending_genai.is_empty() {
            return;
        }

        let pending_items: Vec<_> = self.pending_genai.drain(..).collect();
        let mut still_pending = Vec::new();
        let mut to_export: Vec<Vec<GenAISemanticEvent>> = Vec::new();

        for pending in pending_items {
            if pending.created_at.elapsed() >= PENDING_SESSION_TIMEOUT {
                log::debug!(
                    "Deferred session_id expired for response_id={}, using fallback",
                    pending.response_id
                );
                self.register_retro_session_fixup(pending.pid, &pending.events);
                to_export.push(pending.events);
            } else {
                still_pending.push(pending);
            }
        }

        self.pending_genai = still_pending;

        for events in &to_export {
            self.complete_and_export_deferred_genai(events);
            self.detect_and_store_interruptions(events);
        }
    }

    /// Flush all remaining pending GenAI events (on shutdown).
    ///
    /// No retro fix-up registration here: the process is exiting, so no
    /// further FileWrite can ever arrive to repair a fallback session_id.
    /// The runtime already repaired what it could via
    /// `apply_retro_session_fixup`; the remaining pendings are persisted with
    /// whatever session_id they carry now (real UUID or fallback).
    fn flush_all_pending_genai(&mut self) {
        let pending_items: Vec<_> = self.pending_genai.drain(..).collect();
        for pending in &pending_items {
            log::debug!(
                "Flushing pending GenAI event on shutdown: response_id={}",
                pending.response_id
            );
        }
        for pending in pending_items {
            self.complete_and_export_deferred_genai(&pending.events);
            self.detect_and_store_interruptions(&pending.events);
        }
    }

    /// Immediately flush deferred GenAI events belonging to an exited process.
    ///
    /// The deferral window only exists to wait for a FileWrite event to
    /// register the response_id → session_id mapping (see
    /// `resolve_pending_genai`). Once the process has exited that mapping can
    /// never be written anymore, so waiting out the remaining
    /// PENDING_SESSION_TIMEOUT would only leave the DB row 'pending'
    /// (output_tokens=0, no output_messages) — a grader reading it inside the
    /// window misjudges the finished call as no_final_answer (issue #2032).
    /// Falling back to the response_id-based session_id therefore loses
    /// nothing. Deferred events of other (still-live) pids keep waiting.
    fn flush_deferred_genai_for_pid(&mut self, pid: u32) {
        if self.pending_genai.is_empty() {
            return;
        }

        let pending_items: Vec<_> = self.pending_genai.drain(..).collect();
        let (to_export, still_pending) = take_deferred_genai_for_pid(pending_items, pid);
        self.pending_genai = still_pending;

        // No retro fix-up registration here: the pid is dead, so the FileWrite
        // that would reveal its session mapping can never arrive anymore.
        for events in &to_export {
            self.complete_and_export_deferred_genai(events);
            self.detect_and_store_interruptions(events);
        }
    }

    /// Get reference to aggregator
    pub fn aggregator(&self) -> &Aggregator {
        &self.aggregator
    }

    /// Get mutable reference to aggregator
    pub fn aggregator_mut(&mut self) -> &mut Aggregator {
        &mut self.aggregator
    }

    /// Get reference to analyzer
    pub fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    /// Get reference to storage
    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    /// Get reference to GenAI exporters
    pub fn genai_exporters(&self) -> &[Box<dyn GenAIExporter>] {
        &self.genai_exporters
    }

    /// Add a custom GenAI exporter at runtime
    pub fn add_genai_exporter(&mut self, exporter: Box<dyn GenAIExporter>) {
        log::info!("Registered GenAI exporter: '{}'", exporter.name());
        self.genai_exporters.push(exporter);
    }

    /// Get reference to agent scanner
    pub fn scanner(&self) -> &AgentScanner {
        &self.scanner
    }

    /// Get mutable reference to agent scanner
    pub fn scanner_mut(&mut self) -> &mut AgentScanner {
        &mut self.scanner
    }

    /// Query token usage by time period
    pub fn query_tokens(&self, period: TimePeriod) -> TokenQueryResult {
        let query = TokenQuery::new(self.storage.token());
        query.by_period(period)
    }

    /// Query token usage by last N hours
    pub fn query_tokens_by_hours(&self, hours: u64) -> TokenQueryResult {
        let query = TokenQuery::new(self.storage.token());
        query.by_hours(hours)
    }

    /// Query token usage with comparison
    pub fn query_tokens_with_compare(&self, period: TimePeriod) -> TokenQueryResult {
        let query = TokenQuery::new(self.storage.token());
        query.by_period_with_compare(period)
    }

    /// Query token usage with breakdown
    pub fn query_tokens_with_breakdown(&self, period: TimePeriod) -> TokenQueryResult {
        let query = TokenQuery::new(self.storage.token());
        query.by_period_with_breakdown(period)
    }

    /// Full token query with comparison and breakdown
    pub fn query_tokens_full(&self, period: TimePeriod) -> TokenQueryResult {
        let query = TokenQuery::new(self.storage.token());
        query.full_query(period)
    }

    /// If pending GenAI queue exceeds count or byte limits, flush the oldest
    /// entries with the fallback session_id to prevent unbounded memory growth.
    fn enforce_pending_genai_limits(&mut self) {
        let max_count = self.runtime_limits.pending_genai_max_count.max(1);
        let max_bytes = self.runtime_limits.pending_genai_max_bytes.max(1);

        while !self.pending_genai.is_empty()
            && (self.pending_genai.len() >= max_count || self.pending_genai_bytes >= max_bytes)
        {
            let oldest = self.pending_genai.remove(0);
            self.pending_genai_bytes = self
                .pending_genai_bytes
                .saturating_sub(oldest.estimated_bytes());
            log::warn!(
                "pending_genai limit exceeded (count={}/{}, bytes={}/{}), \
                 flushing oldest response_id={}",
                self.pending_genai.len() + 1,
                max_count,
                self.pending_genai_bytes + oldest.estimated_bytes(),
                max_bytes,
                oldest.response_id
            );
            let events = oldest.events;
            self.complete_and_export_deferred_genai(&events);
            self.detect_and_store_interruptions(&events);
        }
    }

    /// Decide whether an analysis result should be persisted to storage.
    ///
    /// `token_stats` is the only default-on feature; `audit` and
    /// `token_consumption` are gated by their respective feature flags.
    fn should_persist_analysis_result(&self, result: &crate::analyzer::AnalysisResult) -> bool {
        match result {
            crate::analyzer::AnalysisResult::Token(_) => true,
            crate::analyzer::AnalysisResult::Audit(_) => self.features.audit_enabled,
            crate::analyzer::AnalysisResult::TokenConsumption(_) => {
                self.features.token_consumption_enabled
            }
            // Http records, Message, and PromptTokens are always persisted.
            _ => true,
        }
    }

    /// Periodically purge old/oversized interruption DB entries.
    fn maybe_purge_interruption_store(&mut self) {
        let policy = self.storage_config.interruptions;
        if policy.check_interval_secs == 0
            || self.last_interruption_purge.elapsed()
                < std::time::Duration::from_secs(policy.check_interval_secs)
        {
            return;
        }
        self.last_interruption_purge = std::time::Instant::now();
        if let Some(ref istore) = self.interruption_store {
            if let Err(e) =
                istore.purge_old_and_oversized(policy.retention_days, policy.max_db_size_mb)
            {
                log::warn!("Interruption store purge failed: {e}");
            }
        }
    }

    /// Periodically report event-buffer watermarks.
    ///
    /// The tracer runs under `MemoryMax=350M`; when it is killed the journal is
    /// the only evidence left, and a cgroup total says nothing about *which*
    /// buffer grew (#2888).
    fn maybe_log_buffer_watermarks(&mut self) {
        if self.last_watermark_log.elapsed() < std::time::Duration::from_secs(60) {
            return;
        }
        self.last_watermark_log = std::time::Instant::now();

        let (needs_attention, report) =
            watermark_report(self.probes.channel_watermarks(), self.pending_genai.len());
        if needs_attention {
            log::info!("{report}");
        } else {
            log::debug!("{report}");
        }
    }
}

impl Drop for AgentSight {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Complete deferred GenAI events: promote pending DB rows to 'complete',
/// then export to non-SQLite exporters (or FFI).
///
/// Extracted as a free function so the persistence policy is unit-testable
/// without constructing a full `AgentSight` instance.
/// Whether FFI reporting should fall back from LLM to raw-HTTP.
///
/// True when there is at least one event and *every* event is a semantically
/// empty `LLMCall` (no parsed request/response messages). Any non-`LLMCall`
/// event or any non-empty call disables the fallback so a meaningful event is
/// never dropped.
fn events_are_empty_llm(events: &[GenAISemanticEvent]) -> bool {
    !events.is_empty()
        && events.iter().all(|e| match e {
            GenAISemanticEvent::LLMCall(call) => call.is_semantically_empty(),
            _ => false,
        })
}

fn complete_deferred_genai(
    events: &[GenAISemanticEvent],
    sqlite_store: Option<&Arc<GenAISqliteStore>>,
    exporters: &[Box<dyn GenAIExporter>],
    ffi_sender: Option<&FfiEventSender>,
) {
    if let Some(store) = sqlite_store {
        for event in events {
            if let Err(e) = store.complete_pending(event) {
                log::warn!("Failed to complete deferred pending call: {e}");
            }
        }
        if let Some(sender) = ffi_sender {
            for event in events {
                if let GenAISemanticEvent::LLMCall(call) = event {
                    sender.send_llm(call);
                }
            }
        } else {
            for exporter in exporters {
                if exporter.name() != "sqlite" {
                    exporter.export(events);
                }
            }
        }
    } else {
        // No SQLite store — export to all exporters (or FFI)
        if let Some(sender) = ffi_sender {
            for event in events {
                if let GenAISemanticEvent::LLMCall(call) = event {
                    sender.send_llm(call);
                }
            }
        } else {
            for exporter in exporters {
                exporter.export(events);
                log::debug!(
                    "Exported {} GenAI events via '{}'",
                    events.len(),
                    exporter.name()
                );
            }
        }
    }
}

/// Split the deferred queue on process exit: entries of `pid` are selected
/// for immediate fallback completion regardless of how long they have been
/// queued (the exited process can no longer produce the awaited session_id
/// mapping); entries of other pids are kept waiting.
///
/// Extracted as a free function so the exit-flush selection is unit-testable
/// without constructing a full `AgentSight` instance.
fn take_deferred_genai_for_pid(
    pending_items: Vec<PendingGenAI>,
    pid: u32,
) -> (Vec<Vec<GenAISemanticEvent>>, Vec<PendingGenAI>) {
    let mut still_pending = Vec::new();
    let mut to_export: Vec<Vec<GenAISemanticEvent>> = Vec::new();

    for pending in pending_items {
        if pending.pid == pid {
            log::debug!(
                "Deferred session_id flushed on exit of pid={pid} for response_id={}, using fallback",
                pending.response_id
            );
            to_export.push(pending.events);
        } else {
            still_pending.push(pending);
        }
    }

    (to_export, still_pending)
}

/// Collect the call_ids eligible for retroactive session fix-up from a
/// timeout-escaped deferred batch.
///
/// pid 0 is the placeholder used when `pending_info` was missing at queue
/// time; it can never match a real FileWrite pid, so it yields nothing.
/// Extracted as a free function so the registration filter is unit-testable
/// without constructing a full `AgentSight` instance.
fn retro_fixup_call_ids(pid: u32, events: &[GenAISemanticEvent]) -> Vec<String> {
    if pid == 0 {
        return Vec::new();
    }
    events
        .iter()
        .filter_map(|e| match e {
            GenAISemanticEvent::LLMCall(call) => Some(call.call_id.clone()),
            _ => None,
        })
        .collect()
}

/// Retroactively repair the session_id of calls that escaped the deferral
/// window before `pid`'s FileWrite mapping arrived (issue #2059).
///
/// The pid entry is kept while the mapping is still unknown (a later write
/// within `RETRO_FIXUP_WINDOW` may bring it) and consumed once the mapping is
/// available; individual entries older than the window are dropped. Extracted
/// as a free function so the fix-up path is unit-testable without a full
/// `AgentSight` instance (mirrors `record_agent_crash_interruptions`).
///
/// `current_agent` (the pid's owner in `pid_agent_name_cache` at fix-up time)
/// guards against pid reuse: when both the registered and the current agent
/// are known and differ, the pid has been recycled by another process and the
/// entry is dropped. Known residual limitation: a pid wrapped around within
/// the window to a *same-named* traced agent that writes a session file can
/// still be mis-repaired — vanishingly rare, and the double guard (32-hex
/// shape in `update_fallback_session_id` + this agent check) limits the harm
/// to a grouping deviation of orphan fallback rows.
fn apply_retro_session_fixup(
    fixups: &mut lru::LruCache<u32, Vec<RetroFixupEntry>>,
    mapper: &ResponseSessionMapper,
    store: &GenAISqliteStore,
    istore: Option<&InterruptionStore>,
    pid: u32,
    current_agent: Option<&str>,
) {
    // Cheap peek first: the vast majority of FileWrite events belong to pids
    // with no timeout-escaped calls.
    if fixups.peek(&pid).is_none() {
        return;
    }
    let Some(session_id) = mapper.get_session_by_pid(pid).map(str::to_string) else {
        return;
    };
    let Some(entries) = fixups.pop(&pid) else {
        return;
    };
    // Entries whose UPDATE errored are re-registered below so a later write
    // retries them; the window check above them bounds the retries.
    let mut failed: Vec<RetroFixupEntry> = Vec::new();
    for (call_id, registered_at, entry_agent) in entries {
        if registered_at.elapsed() > RETRO_FIXUP_WINDOW {
            continue;
        }
        if let (Some(registered), Some(current)) = (entry_agent.as_deref(), current_agent) {
            if registered != current {
                log::debug!(
                    "retro session fix-up skipped for call_id={call_id}: pid {pid} was reused (agent {registered} -> {current})"
                );
                continue;
            }
        }
        // Repair the interruptions of this call too: they were persisted with
        // a NULL session_id while the call was deferred, and the per-session
        // dashboard breakdown would otherwise never attribute them.
        //
        // Both updates are idempotent (one targets NULL session_id, the other
        // the 32-hex fallback shape), so re-registering on partial failure is
        // safe and is required: dropping the entry after only one store
        // succeeded would leave the other permanently unrepaired.
        let interruption_repaired = match istore {
            Some(istore) => match istore.backfill_null_session_id(&call_id, &session_id) {
                Ok(n) => {
                    if n > 0 {
                        log::debug!(
                            "retro session fix-up: {n} interruption(s) of call_id={call_id} -> session_id={session_id}"
                        );
                    }
                    true
                }
                Err(e) => {
                    log::warn!(
                        "retro session fix-up of interruptions failed for call_id={call_id}: {e}"
                    );
                    false
                }
            },
            None => true,
        };
        let genai_repaired = match store.update_fallback_session_id(&call_id, &session_id) {
            Ok(n) => {
                if n > 0 {
                    log::debug!("retro session fix-up: call_id={call_id} session_id={session_id}");
                }
                true
            }
            Err(e) => {
                log::warn!("retro session fix-up failed for call_id={call_id}: {e}");
                false
            }
        };
        if !interruption_repaired || !genai_repaired {
            failed.push((call_id, registered_at, entry_agent));
        }
    }
    if !failed.is_empty() {
        fixups.put(pid, failed);
    }
}

/// Record `agent_crash` interruption events for the pending calls of an
/// exited agent process, and mark those calls as interrupted.
///
/// Skips entirely when the process terminated without a crash: a voluntary
/// exit with code 0, or a graceful parent-initiated reap via
/// SIGTERM/SIGINT/SIGHUP with no core dump (a single-shot agent finishing its
/// run with still-unresolved pending calls is not a crash; issue #1989).
/// Abnormal exits (non-zero code, fatal signal, core dump, or the SIGKILL
/// sent by the OOM killer) keep the previous behavior, with the decoded exit
/// status embedded in the detail JSON.
///
/// Extracted as a free function so the crash decision is unit-testable
/// without constructing a full `AgentSight` instance.
fn record_agent_crash_interruptions(
    pid: u32,
    agent_name: &str,
    exit_status: ProcessExitStatus,
    pending_calls: &[(String, Option<String>, Option<String>, Option<String>)],
    istore: &InterruptionStore,
    genai_store: Option<&GenAISqliteStore>,
) {
    use crate::interruption::{
        InterruptionEvent, InterruptionType, is_reap_worker_agent, was_pid_oom_killed,
    };

    // Nothing to record without pending calls; guard here so future callers
    // cannot emit call-less crash events by accident.
    if pending_calls.is_empty() {
        return;
    }

    let clean_exit = exit_status.is_clean()
        || (exit_status.is_graceful_reap() && is_reap_worker_agent(agent_name));
    if clean_exit {
        log::info!(
            "[CrashDetect] Agent {agent_name} (pid={pid}) exited cleanly with {} pending call(s) — not a crash",
            pending_calls.len(),
        );
        return;
    }

    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    let is_oom = was_pid_oom_killed(pid as i32);

    // Group by (session_id, conversation_id) to produce one event per conversation
    let mut by_conv: std::collections::HashMap<(Option<String>, Option<String>), Vec<String>> =
        std::collections::HashMap::new();
    for (call_id, session_id, _trace_id, conversation_id) in pending_calls {
        by_conv
            .entry((session_id.clone(), conversation_id.clone()))
            .or_default()
            .push(call_id.clone());
    }

    for ((session_id, conversation_id), call_ids) in &by_conv {
        let mut detail = serde_json::json!({
            "pid": pid,
            "agent_name": agent_name,
            "call_ids": call_ids,
            "source": "trace_procmon_exit",
            "exit_code": exit_status.code,
            "signal": exit_status.signal,
            "core_dump": exit_status.core_dump,
        });
        if is_oom {
            detail["oom"] = serde_json::json!(true);
        }
        let event = InterruptionEvent::new(
            InterruptionType::AgentCrash,
            session_id.clone(),
            None,
            conversation_id.clone(),
            None,
            Some(pid as i32),
            Some(agent_name.to_string()),
            now_ns,
            Some(detail),
        );
        if let Err(e) = istore.insert(&event) {
            log::warn!("[CrashDetect] Failed to record agent_crash for pid={pid}: {e}");
        } else {
            log::info!(
                "[CrashDetect] Recorded agent_crash for {} (pid={}, session={:?}, conversation={:?}, {} call(s), exit_code={}, signal={}, oom={})",
                agent_name,
                pid,
                session_id,
                conversation_id,
                call_ids.len(),
                exit_status.code,
                exit_status.signal,
                is_oom,
            );
        }
        crate::genai::logtail::export_interruption_events(std::slice::from_ref(&event));
    }

    // Mark all pending calls for this PID as interrupted
    if let Some(store) = genai_store {
        let itype = if is_oom { "oom_crash" } else { "agent_crash" };
        if let Err(e) = store.mark_pending_interrupted_for_pid(pid as i32, itype) {
            log::warn!("[CrashDetect] Failed to mark pending interrupted for pid={pid}: {e}");
        }
    }
}

/// Render the buffer watermark report, and whether it needs operator attention.
///
/// Split out from the logging call site so the wording is covered by tests: this
/// line is the only evidence left after an OOM kill, and it has to say which
/// buffer holds the memory. Attention means the byte budget already rejected
/// events, which is reported at INFO so diagnosing an OOM does not require
/// turning on debug logging; a quiet pipeline stays at DEBUG.
fn watermark_report(marks: ChannelWatermarks, pending_genai: usize) -> (bool, String) {
    let mut report = format!(
        "Buffer watermarks: event channel {} / {} bytes in flight",
        marks.in_flight_bytes, marks.budget_bytes
    );
    let needs_attention = marks.dropped_over_budget > 0;
    if needs_attention {
        report.push_str(&format!(
            ", {} events dropped over budget",
            marks.dropped_over_budget
        ));
    }
    report.push_str(&format!(", pending_genai {pending_genai} entries"));
    (needs_attention, report)
}

#[cfg(test)]
mod response_pending_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn watermark_report_stays_quiet_without_drops() {
        let (needs_attention, report) = watermark_report(
            ChannelWatermarks {
                in_flight_bytes: 1024,
                budget_bytes: 64 * 1024 * 1024,
                dropped_over_budget: 0,
            },
            3,
        );
        assert!(!needs_attention);
        assert_eq!(
            report,
            "Buffer watermarks: event channel 1024 / 67108864 bytes in flight, \
             pending_genai 3 entries"
        );
    }

    #[test]
    fn watermark_report_surfaces_drops() {
        // The drop count is the signal that separates channel pressure from
        // downstream growth, so it must appear and must raise the level.
        let (needs_attention, report) = watermark_report(
            ChannelWatermarks {
                in_flight_bytes: 67108864,
                budget_bytes: 67108864,
                dropped_over_budget: 42,
            },
            17,
        );
        assert!(needs_attention);
        assert!(report.contains("42 events dropped over budget"));
        assert!(report.contains("67108864 / 67108864 bytes in flight"));
        assert!(report.contains("pending_genai 17 entries"));
    }

    #[test]
    fn watermark_report_shows_unlimited_budget_as_zero() {
        // With the gate disabled the reading must still be usable, otherwise an
        // operator who set 0 loses the only in-flight measurement.
        let (needs_attention, report) = watermark_report(
            ChannelWatermarks {
                in_flight_bytes: 8 * 1024 * 1024,
                budget_bytes: 0,
                dropped_over_budget: 0,
            },
            0,
        );
        assert!(!needs_attention);
        assert!(report.contains("8388608 / 0 bytes in flight"));
    }

    #[test]
    fn resource_sampler_is_not_started_when_disabled() {
        let targets = Arc::new(RwLock::new(HashMap::new()));
        let running = Arc::new(AtomicBool::new(true));
        assert!(
            AgentSight::start_resource_sampler_if_enabled(false, None, targets, running).is_none()
        );
    }

    #[test]
    fn resource_sampler_is_not_started_without_sqlite() {
        let targets = Arc::new(RwLock::new(HashMap::new()));
        let running = Arc::new(AtomicBool::new(true));
        assert!(
            AgentSight::start_resource_sampler_if_enabled(true, None, targets, running).is_none()
        );
    }

    #[test]
    fn resource_sampler_starts_when_enabled_with_sqlite() {
        let dir = unique_tmp_dir("resource-sampler");
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &dir.join("genai_events.db"),
                crate::config::InsertStoragePolicy::default(),
            )
            .expect("genai store"),
        );
        let targets = Arc::new(RwLock::new(HashMap::new()));
        let running = Arc::new(AtomicBool::new(true));
        let handle = AgentSight::start_resource_sampler_if_enabled(
            true,
            Some(&store),
            targets,
            Arc::clone(&running),
        )
        .expect("resource sampler handle");
        running.store(false, Ordering::SeqCst);
        handle.join().expect("resource sampler join");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Generate a unique temp directory for each test invocation.
    pub(super) fn unique_tmp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("agentsight-tc-{pid}-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    // ── Tests for record_agent_crash_interruptions (issue #1989) ──

    /// Build the stores + one pending call for `pid`, returning the paths so
    /// tests can assert on both databases.
    fn setup_crash_stores(
        tag: &str,
        pid: i32,
    ) -> (PathBuf, Arc<GenAISqliteStore>, Arc<InterruptionStore>) {
        let dir = unique_tmp_dir(tag);
        let genai_store = Arc::new(
            GenAISqliteStore::new_with_path(
                &dir.join("genai_events.db"),
                crate::config::InsertStoragePolicy::default(),
            )
            .expect("genai store"),
        );
        let istore = Arc::new(
            InterruptionStore::new_with_path(&dir.join("interruption_events.db"))
                .expect("interruption store"),
        );
        let mut info = make_test_pending_info("crash-call-1");
        info.pid = pid;
        genai_store.insert_pending(&info).expect("insert_pending");
        (dir, genai_store, istore)
    }

    fn list_crash_events(
        istore: &InterruptionStore,
    ) -> Vec<crate::storage::sqlite::interruption::InterruptionRecord> {
        istore
            .list(0, i64::MAX, None, Some("agent_crash"), None, None, 100)
            .expect("list interruptions")
    }

    #[test]
    fn test_clean_exit_with_pending_call_records_no_agent_crash() {
        // Use a PID that cannot appear in dmesg OOM logs.
        let pid = 3_999_991;
        let (dir, genai_store, istore) = setup_crash_stores("clean-exit", pid);

        let pending = genai_store
            .list_pending_for_pids(&[pid])
            .expect("list pending");
        assert_eq!(pending.len(), 1, "precondition: one pending call");

        // exit 0 → raw exit_code 0x0 (measured on a real kernel)
        record_agent_crash_interruptions(
            pid as u32,
            "cosh-core",
            ProcessExitStatus::decode(0x0),
            &pending,
            &istore,
            Some(genai_store.as_ref()),
        );

        assert!(
            list_crash_events(&istore).is_empty(),
            "clean exit must not produce an agent_crash interruption"
        );
        // The pending call must NOT be stamped as interrupted either — that
        // column is what pollutes grader root_cause attribution.
        assert_eq!(
            genai_store
                .list_pending_for_pids(&[pid])
                .expect("list pending")
                .len(),
            1,
            "pending call must stay pending on clean exit"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sigterm_reap_with_pending_call_records_no_agent_crash() {
        // A known per-request worker family (CoshNG) reaped with SIGTERM (raw
        // 0x0f) is a deliberate graceful shutdown: no agent_crash may be
        // recorded even when the process left pending calls behind.
        let pid = 3_999_994;
        let (dir, genai_store, istore) = setup_crash_stores("sigterm-exit", pid);
        let pending = genai_store
            .list_pending_for_pids(&[pid])
            .expect("list pending");

        record_agent_crash_interruptions(
            pid as u32,
            "CoshNG",
            ProcessExitStatus::decode(0x0f),
            &pending,
            &istore,
            Some(genai_store.as_ref()),
        );

        assert!(
            list_crash_events(&istore).is_empty(),
            "SIGTERM reap must not produce an agent_crash interruption"
        );
        // The pending call must NOT be stamped as interrupted either.
        assert_eq!(
            genai_store
                .list_pending_for_pids(&[pid])
                .expect("list pending")
                .len(),
            1,
            "pending call must stay pending on graceful reap"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sigterm_non_worker_agent_still_records_agent_crash() {
        // The graceful-reap exemption is limited to known worker families. A
        // non-worker agent (Codex) interrupted by SIGTERM with a pending call
        // is still a mid-session disappearance and must record a crash.
        let pid = 3_999_995;
        let (dir, genai_store, istore) = setup_crash_stores("sigterm-codex", pid);
        let pending = genai_store
            .list_pending_for_pids(&[pid])
            .expect("list pending");

        record_agent_crash_interruptions(
            pid as u32,
            "Codex",
            ProcessExitStatus::decode(0x0f),
            &pending,
            &istore,
            Some(genai_store.as_ref()),
        );

        assert_eq!(
            list_crash_events(&istore).len(),
            1,
            "SIGTERM on a non-worker agent must still record agent_crash"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sigkill_exit_records_agent_crash_with_signal_detail() {
        let pid = 3_999_992;
        let (dir, genai_store, istore) = setup_crash_stores("sigkill-exit", pid);
        let pending = genai_store
            .list_pending_for_pids(&[pid])
            .expect("list pending");

        // SIGKILL → raw exit_code 0x9 (measured on a real kernel)
        record_agent_crash_interruptions(
            pid as u32,
            "cosh-core",
            ProcessExitStatus::decode(0x9),
            &pending,
            &istore,
            Some(genai_store.as_ref()),
        );

        let events = list_crash_events(&istore);
        assert_eq!(events.len(), 1, "SIGKILL must produce one agent_crash");
        let detail: serde_json::Value =
            serde_json::from_str(events[0].detail.as_deref().expect("detail json"))
                .expect("parse detail");
        assert_eq!(detail["signal"], 9);
        assert_eq!(detail["exit_code"], 0);
        assert_eq!(detail["core_dump"], false);
        assert_eq!(detail["source"], "trace_procmon_exit");

        // Pending call is marked interrupted on the crash path.
        assert!(
            genai_store
                .list_pending_for_pids(&[pid])
                .expect("list pending")
                .is_empty(),
            "crash path must mark the pending call as interrupted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nonzero_exit_code_records_agent_crash_with_code_detail() {
        let pid = 3_999_993;
        let (dir, genai_store, istore) = setup_crash_stores("exit3", pid);
        let pending = genai_store
            .list_pending_for_pids(&[pid])
            .expect("list pending");

        // exit 3 → raw exit_code 0x300 (measured on a real kernel)
        record_agent_crash_interruptions(
            pid as u32,
            "cosh-core",
            ProcessExitStatus::decode(0x300),
            &pending,
            &istore,
            Some(genai_store.as_ref()),
        );

        let events = list_crash_events(&istore);
        assert_eq!(events.len(), 1, "exit 3 must produce one agent_crash");
        let detail: serde_json::Value =
            serde_json::from_str(events[0].detail.as_deref().expect("detail json"))
                .expect("parse detail");
        assert_eq!(detail["exit_code"], 3);
        assert_eq!(detail["signal"], 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sigsegv_core_dump_records_agent_crash_with_core_detail() {
        let pid = 3_999_994;
        let (dir, genai_store, istore) = setup_crash_stores("segv-core", pid);
        let pending = genai_store
            .list_pending_for_pids(&[pid])
            .expect("list pending");

        // SIGSEGV with core dump → raw exit_code 0x8b (measured on a real kernel)
        record_agent_crash_interruptions(
            pid as u32,
            "cosh-core",
            ProcessExitStatus::decode(0x8b),
            &pending,
            &istore,
            Some(genai_store.as_ref()),
        );

        let events = list_crash_events(&istore);
        assert_eq!(events.len(), 1, "SIGSEGV must produce one agent_crash");
        let detail: serde_json::Value =
            serde_json::from_str(events[0].detail.as_deref().expect("detail json"))
                .expect("parse detail");
        assert_eq!(detail["signal"], 11);
        assert_eq!(detail["core_dump"], true);
        assert_eq!(detail["exit_code"], 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Tests for conn_scan_agent_name (agent identity, never a domain) ──

    #[test]
    fn test_conn_scan_agent_name_uses_matched_rule() {
        // When a cmdline rule matches, the configured agent name is returned.
        let rules = vec![crate::config::CmdlineRule {
            patterns: vec!["*".to_string()],
            agent_name: Some("MatchedAgent".to_string()),
            allow: true,
        }];
        let scanner = AgentScanner::from_rules(&rules, &[]);
        let name = AgentSight::conn_scan_agent_name(&scanner, std::process::id());
        assert_eq!(name, Some("MatchedAgent".to_string()));
    }

    #[test]
    fn test_conn_scan_agent_name_falls_back_to_process_comm() {
        // No matching rule → the process comm, never a "domain:<host>" string.
        let scanner = AgentScanner::from_rules(&[], &[]);
        let name = AgentSight::conn_scan_agent_name(&scanner, std::process::id());
        assert!(name.is_some());
        assert!(!name.unwrap().starts_with("domain:"));
    }

    #[test]
    fn test_conn_scan_agent_name_none_for_dead_pid() {
        // Unmatched and unreadable comm → None (left uncached).
        let scanner = AgentScanner::from_rules(&[], &[]);
        assert!(AgentSight::conn_scan_agent_name(&scanner, u32::MAX).is_none());
    }

    // ── Tests for complete_deferred_genai + complete_pending guard ──

    /// Stub exporter that records exported events for assertion.
    struct RecordingExporter {
        name: String,
        events: std::sync::Mutex<Vec<GenAISemanticEvent>>,
    }

    impl RecordingExporter {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl GenAIExporter for RecordingExporter {
        fn name(&self) -> &str {
            &self.name
        }
        fn export(&self, events: &[GenAISemanticEvent]) {
            self.events.lock().unwrap().extend_from_slice(events);
        }
    }

    fn make_test_llm_call(call_id: &str) -> crate::genai::LLMCall {
        use crate::genai::semantic::{LLMRequest, LLMResponse};
        crate::genai::LLMCall {
            call_id: call_id.to_string(),
            start_timestamp_ns: 1_000_000_000,
            end_timestamp_ns: 2_000_000_000,
            duration_ns: 1_000_000_000,
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            request: LLMRequest {
                messages: vec![],
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
            },
            response: LLMResponse {
                messages: vec![],
                streamed: false,
                raw_body: None,
            },
            token_usage: None,
            error: None,
            pid: 1234,
            process_name: "test".to_string(),
            agent_name: Some("test-agent".to_string()),
            metadata: std::collections::HashMap::new(),
        }
    }

    fn make_test_pending_info(call_id: &str) -> crate::storage::sqlite::genai::PendingCallInfo {
        crate::storage::sqlite::genai::PendingCallInfo {
            call_id: call_id.to_string(),
            trace_id: None,
            conversation_id: None,
            session_id: None,
            start_timestamp_ns: 1_000_000_000,
            pid: 1234,
            process_name: "test".to_string(),
            agent_name: Some("test-agent".to_string()),
            http_method: Some("POST".to_string()),
            http_path: Some("/v1/chat/completions".to_string()),
            input_messages: None,
            system_instructions: None,
            user_query: None,
            is_sse: false,
            model: Some("gpt-4".to_string()),
            provider: Some("openai".to_string()),
            call_kind: "main".to_string(),
            pending_origin: crate::storage::sqlite::genai::PendingOrigin::RequestCapture,
            pending_match_key: None,
        }
    }

    #[test]
    fn test_complete_pending_skips_insert_when_row_already_interrupted() {
        let dir = unique_tmp_dir("cp-interrupted");
        let db_path = dir.join("genai_events.db");
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &db_path,
                crate::config::InsertStoragePolicy::default(),
            )
            .expect("create test store"),
        );

        let info = make_test_pending_info("call-1");
        store.insert_pending(&info).expect("insert_pending");

        // Simulate crash detection marking it as interrupted
        store
            .mark_pending_interrupted_for_pid(1234, "agent_crash")
            .expect("mark interrupted");

        // Now complete_pending should NOT create a duplicate row
        let event = GenAISemanticEvent::LLMCall(make_test_llm_call("call-1"));
        store.complete_pending(&event).expect("complete_pending");

        // Verify: exactly 1 row (no duplicate), promoted to complete.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM genai_events WHERE call_id = 'call-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "should have exactly 1 row, not 2 (double-write)");

        let status: String = conn
            .query_row(
                "SELECT status FROM genai_events WHERE call_id = 'call-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "complete");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_complete_pending_fallback_inserts_when_no_row_exists() {
        let dir = unique_tmp_dir("cp-fallback");
        let db_path = dir.join("genai_events.db");
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &db_path,
                crate::config::InsertStoragePolicy::default(),
            )
            .expect("create test store"),
        );

        // No insert_pending — simulate DB restart scenario
        let event = GenAISemanticEvent::LLMCall(make_test_llm_call("call-2"));
        store.complete_pending(&event).expect("complete_pending");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM genai_events WHERE call_id = 'call-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "fallback INSERT should create exactly 1 row");

        let status: String = conn
            .query_row(
                "SELECT status FROM genai_events WHERE call_id = 'call-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "complete");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_complete_deferred_genai_promotes_pending_and_exports_non_sqlite() {
        let dir = unique_tmp_dir("deferred-export");
        let db_path = dir.join("genai_events.db");
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &db_path,
                crate::config::InsertStoragePolicy::default(),
            )
            .expect("create test store"),
        );

        // Insert a pending row
        let info = make_test_pending_info("call-3");
        store.insert_pending(&info).expect("insert_pending");

        // Build event + exporters
        let event = GenAISemanticEvent::LLMCall(make_test_llm_call("call-3"));
        let recorder = RecordingExporter::new("test-recorder");
        let sqlite_exporter = RecordingExporter::new("sqlite");
        let exporters: Vec<Box<dyn GenAIExporter>> =
            vec![Box::new(recorder), Box::new(sqlite_exporter)];

        // Call the free function
        complete_deferred_genai(&[event], Some(&store), &exporters, None);

        // DB row should be promoted to 'complete'
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM genai_events WHERE call_id = 'call-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "complete");

        // test-recorder should have received the event; sqlite exporter should NOT
        // (We can't inspect after move, but the function skips name()=="sqlite")
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM genai_events WHERE call_id = 'call-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "exactly 1 row (no double-write from sqlite exporter)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_complete_deferred_genai_no_sqlite_exports_to_all() {
        let event = GenAISemanticEvent::LLMCall(make_test_llm_call("call-4"));
        let exporters: Vec<Box<dyn GenAIExporter>> =
            vec![Box::new(RecordingExporter::new("test-recorder"))];

        complete_deferred_genai(&[event], None, &exporters, None);
    }

    #[test]
    fn test_pending_genai_estimated_bytes() {
        let pending = PendingGenAI {
            events: vec![],
            response_id: "test-response-id".to_string(),
            pid: 1234,
            created_at: std::time::Instant::now(),
        };
        let bytes = pending.estimated_bytes();
        // Should include struct size + response_id length
        assert!(bytes >= std::mem::size_of::<PendingGenAI>() + 16);
        // Empty events should contribute 0 extra
        assert_eq!(
            bytes,
            std::mem::size_of::<PendingGenAI>() + "test-response-id".len()
        );
    }

    #[test]
    fn test_pending_genai_estimated_bytes_with_events() {
        let event = GenAISemanticEvent::LLMCall(make_test_llm_call("call-x"));
        let pending = PendingGenAI {
            events: vec![event],
            response_id: "r".to_string(),
            pid: 1,
            created_at: std::time::Instant::now(),
        };
        let bytes = pending.estimated_bytes();
        // 1 event × 512 bytes estimate
        assert!(bytes >= std::mem::size_of::<PendingGenAI>() + 1 + 512);
    }

    // ── Test for exit-time deferred flush (issue #2032) ──

    #[test]
    fn test_exit_flush_completes_deferred_call_without_waiting_timeout() {
        let dir = unique_tmp_dir("exit-flush");
        let db_path = dir.join("genai_events.db");
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &db_path,
                crate::config::InsertStoragePolicy::default(),
            )
            .expect("create test store"),
        );

        // Pending rows for both pids, as written at deferred-queue time.
        let mut exiting = make_test_pending_info("exit-flush-call");
        exiting.pid = 4242;
        store.insert_pending(&exiting).expect("insert_pending");
        let mut other = make_test_pending_info("other-pid-call");
        other.pid = 5555;
        store.insert_pending(&other).expect("insert_pending");

        // The exiting pid's deferred event already carries the full response.
        let mut call = make_test_llm_call("exit-flush-call");
        call.token_usage = Some(crate::genai::semantic::TokenUsage {
            input_tokens: 11,
            output_tokens: 7,
            total_tokens: 18,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        let entries = vec![
            PendingGenAI {
                events: vec![GenAISemanticEvent::LLMCall(call)],
                response_id: "resp-exit".to_string(),
                pid: 4242,
                // Fresh entry: the exit flush must not wait for
                // PENDING_SESSION_TIMEOUT (the old behavior, which left the
                // row pending for up to 5s and let the grader misjudge it).
                created_at: std::time::Instant::now(),
            },
            PendingGenAI {
                events: vec![GenAISemanticEvent::LLMCall(make_test_llm_call(
                    "other-pid-call",
                ))],
                response_id: "resp-other".to_string(),
                pid: 5555,
                created_at: std::time::Instant::now(),
            },
        ];

        // Exit of pid 4242: selection ignores created_at, keeps other pids.
        let (flushed, kept) = take_deferred_genai_for_pid(entries, 4242);
        assert_eq!(flushed.len(), 1, "exiting pid's events flush immediately");
        assert_eq!(kept.len(), 1, "other pid's events keep waiting");
        assert_eq!(kept[0].pid, 5555);

        // Completing the flushed batches promotes the row with real output.
        for events in &flushed {
            complete_deferred_genai(events, Some(&store), &[], None);
        }

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let (status, output_tokens): (String, i64) = conn
            .query_row(
                "SELECT status, output_tokens FROM genai_events WHERE call_id = 'exit-flush-call'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "complete", "complete right at exit, not after 5s");
        assert_eq!(
            output_tokens, 7,
            "no no_final_answer precondition left (output_tokens > 0)"
        );

        // The untouched pid's row stays pending — per-pid selection only.
        let other_status: String = conn
            .query_row(
                "SELECT status FROM genai_events WHERE call_id = 'other-pid-call'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(other_status, "pending");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Tests for the AgentsightHttpsData fallback path ──

    #[test]
    fn test_events_are_empty_llm() {
        use crate::genai::semantic::{InputMessage, MessagePart};

        // No events → no fallback.
        assert!(!events_are_empty_llm(&[]));

        // A single semantically empty LLM call → fallback.
        let empty = GenAISemanticEvent::LLMCall(make_test_llm_call("c1"));
        assert!(events_are_empty_llm(std::slice::from_ref(&empty)));

        // A call with parsed request messages → no fallback.
        let mut call = make_test_llm_call("c2");
        call.request.messages.push(InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: "hi".to_string(),
            }],
            name: None,
        });
        assert!(!events_are_empty_llm(&[GenAISemanticEvent::LLMCall(call)]));

        // A non-LLM event alongside an empty call disables the fallback.
        let tool = GenAISemanticEvent::ToolUse(crate::genai::semantic::ToolUse {
            tool_use_id: "t1".to_string(),
            timestamp_ns: 0,
            tool_name: "grep".to_string(),
            arguments: serde_json::Value::Null,
            result: None,
            duration_ns: None,
            success: true,
            error: None,
            parent_llm_call_id: None,
            pid: 1,
        });
        assert!(!events_are_empty_llm(&[empty, tool]));
    }

    // ── Tests for retroactive session fix-up (issue #2059) ──

    /// Build a mapper that learned `pid` → the cosh session UUID from a
    /// FileWrite of the atomic-write temp file, exactly as at runtime.
    fn make_mapper_with_pid(pid: u32) -> ResponseSessionMapper {
        let mut mapper = ResponseSessionMapper::new();
        mapper.process_filewrite(&FileWriteEvent {
            pid,
            tid: pid,
            uid: 0,
            timestamp_ns: 0,
            write_size: 0,
            comm: "cosh-core".to_string(),
            filename:
                ".550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp"
                    .to_string(),
            cgroup_id: 0,
            buf: Vec::new(),
        });
        mapper
    }

    fn make_fixup_cache() -> lru::LruCache<u32, Vec<RetroFixupEntry>> {
        // Capacity constant is non-zero, unwrap cannot fire.
        lru::LruCache::new(std::num::NonZeroUsize::new(RETRO_FIXUP_CAPACITY).unwrap())
    }

    #[test]
    fn test_retro_fixup_call_ids_skips_pid_zero() {
        let events = vec![GenAISemanticEvent::LLMCall(make_test_llm_call("call-a"))];
        // pid 0 is the missing-pending_info placeholder — never registered.
        assert!(retro_fixup_call_ids(0, &events).is_empty());
        assert_eq!(retro_fixup_call_ids(4242, &events), vec!["call-a"]);
    }

    /// End-to-end narrow chain: a call escapes the deferral window with a
    /// hash fallback session_id, gets registered, and a later FileWrite from
    /// the same pid repairs the DB row. Reverting the fix-up makes the final
    /// session_id assertion fail (the row would keep the 32-hex fallback).
    #[test]
    fn test_retro_session_fixup_repairs_timeout_escaped_call() {
        let dir = unique_tmp_dir("retro-fixup");
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai_events.db"),
            crate::config::InsertStoragePolicy::default(),
        )
        .expect("genai store");
        let mut info = make_test_pending_info("retro-call-1");
        info.pid = 4242;
        // The 32-hex shape the id_resolver fallback produces.
        info.session_id = Some("0123456789abcdef0123456789abcdef".to_string());
        store.insert_pending(&info).expect("insert_pending");

        let mut fixups = make_fixup_cache();
        fixups.put(
            4242,
            vec![("retro-call-1".to_string(), std::time::Instant::now(), None)],
        );

        let mapper = make_mapper_with_pid(4242);
        apply_retro_session_fixup(&mut fixups, &mapper, &store, None, 4242, None);

        let rows = store.list_pending_for_pids(&[4242]).expect("list pending");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000"),
            "fallback session_id must be repaired to the mapper UUID"
        );
        assert!(
            fixups.peek(&4242).is_none(),
            "pid entry must be consumed after the fix-up"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cross-module coverage for the dual-store fix-up: one late FileWrite must
    /// repair the GenAI row **and** the interruptions of the same call. Passing
    /// `None` for the interruption store (as the narrow test above does) cannot
    /// catch a regression in the interruption branch.
    #[test]
    fn test_retro_session_fixup_repairs_both_stores() {
        use crate::interruption::{InterruptionEvent, InterruptionType};

        let dir = unique_tmp_dir("retro-fixup-dual");
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai_events.db"),
            crate::config::InsertStoragePolicy::default(),
        )
        .expect("genai store");
        let istore = InterruptionStore::new_with_path(&dir.join("interruption_events.db"))
            .expect("interruption store");

        let mut info = make_test_pending_info("retro-call-dual");
        info.pid = 4242;
        info.session_id = Some("0123456789abcdef0123456789abcdef".to_string());
        store.insert_pending(&info).expect("insert_pending");

        // An interruption detected while the call was still deferred: no
        // session_id could be attached at detection time.
        let mut event = InterruptionEvent::new(
            InterruptionType::EmptyResponse,
            None,
            None,
            Some("conv-dual".to_string()),
            Some("retro-call-dual".to_string()),
            Some(4242),
            Some("cosh-core".to_string()),
            1_000,
            None,
        );
        event.interruption_id = "int-retro-dual".to_string();
        istore.insert(&event).expect("insert interruption");

        let mut fixups = make_fixup_cache();
        fixups.put(
            4242,
            vec![(
                "retro-call-dual".to_string(),
                std::time::Instant::now(),
                None,
            )],
        );

        let mapper = make_mapper_with_pid(4242);
        apply_retro_session_fixup(&mut fixups, &mapper, &store, Some(&istore), 4242, None);

        const RESOLVED: &str = "550e8400-e29b-41d4-a716-446655440000";
        let rows = store.list_pending_for_pids(&[4242]).expect("list pending");
        assert_eq!(rows[0].1.as_deref(), Some(RESOLVED), "genai row repaired");

        let repaired = istore
            .get_by_id("int-retro-dual")
            .expect("get interruption")
            .expect("interruption present");
        assert_eq!(
            repaired.session_id.as_deref(),
            Some(RESOLVED),
            "interruption session_id must be backfilled by the same fix-up"
        );

        // Repaired rows must now appear in the per-session breakdown under the
        // real session rather than the unassigned bucket.
        let breakdown = istore
            .count_unresolved_by_session_detailed(0, i64::MAX, None)
            .expect("breakdown");
        assert!(
            breakdown.iter().any(|(sid, _, _, _)| sid == RESOLVED),
            "backfilled interruption must group under the resolved session"
        );
        assert!(
            !breakdown
                .iter()
                .any(|(sid, _, _, _)| sid == crate::storage::sqlite::UNASSIGNED_SESSION_ID),
            "unassigned bucket must be empty once the session is known"
        );
        assert!(
            fixups.peek(&4242).is_none(),
            "pid entry must be consumed after a fully successful fix-up"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A FileWrite that does not yield a mapping for the pid must keep the
    /// registration for later writes and leave the row untouched.
    #[test]
    fn test_retro_session_fixup_waits_for_mapping() {
        let dir = unique_tmp_dir("retro-wait");
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai_events.db"),
            crate::config::InsertStoragePolicy::default(),
        )
        .expect("genai store");
        let mut info = make_test_pending_info("retro-call-2");
        info.pid = 4242;
        info.session_id = Some("0123456789abcdef0123456789abcdef".to_string());
        store.insert_pending(&info).expect("insert_pending");

        let mut fixups = make_fixup_cache();
        fixups.put(
            4242,
            vec![("retro-call-2".to_string(), std::time::Instant::now(), None)],
        );

        // Mapper knows a different pid only.
        let mapper = make_mapper_with_pid(9999);
        apply_retro_session_fixup(&mut fixups, &mapper, &store, None, 4242, None);

        let rows = store.list_pending_for_pids(&[4242]).expect("list pending");
        assert_eq!(
            rows[0].1.as_deref(),
            Some("0123456789abcdef0123456789abcdef"),
            "row must stay on the fallback while the mapping is unknown"
        );
        assert!(
            fixups.peek(&4242).is_some(),
            "entry must survive until the mapping arrives or expires"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Entries older than RETRO_FIXUP_WINDOW must be dropped without touching
    /// the DB, even when the mapping is available.
    #[test]
    fn test_retro_session_fixup_drops_expired_entries() {
        // Back-date the registration beyond the window. On hosts whose
        // monotonic clock started less than the window ago this cannot be
        // represented — skip silently (Instant cannot be mocked).
        let Some(expired_at) = std::time::Instant::now()
            .checked_sub(RETRO_FIXUP_WINDOW + std::time::Duration::from_secs(1))
        else {
            return;
        };

        let dir = unique_tmp_dir("retro-expired");
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai_events.db"),
            crate::config::InsertStoragePolicy::default(),
        )
        .expect("genai store");
        let mut info = make_test_pending_info("retro-call-3");
        info.pid = 4242;
        info.session_id = Some("0123456789abcdef0123456789abcdef".to_string());
        store.insert_pending(&info).expect("insert_pending");

        let mut fixups = make_fixup_cache();
        fixups.put(4242, vec![("retro-call-3".to_string(), expired_at, None)]);

        let mapper = make_mapper_with_pid(4242);
        apply_retro_session_fixup(&mut fixups, &mapper, &store, None, 4242, None);

        let rows = store.list_pending_for_pids(&[4242]).expect("list pending");
        assert_eq!(
            rows[0].1.as_deref(),
            Some("0123456789abcdef0123456789abcdef"),
            "expired entries must not touch the DB"
        );
        assert!(
            fixups.peek(&4242).is_none(),
            "expired entries are discarded together with the pid entry"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pid recycled by a different traced agent must not have its old
    /// entries repaired: registered agent and current agent both known but
    /// different → drop, no DB write. Reverting the agent guard in
    /// `apply_retro_session_fixup` makes this test fail (the row would be
    /// repaired with the new process's session).
    #[test]
    fn test_retro_session_fixup_skips_reused_pid() {
        let dir = unique_tmp_dir("retro-pid-reuse");
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai_events.db"),
            crate::config::InsertStoragePolicy::default(),
        )
        .expect("genai store");
        let mut info = make_test_pending_info("retro-call-4");
        info.pid = 4242;
        info.session_id = Some("0123456789abcdef0123456789abcdef".to_string());
        store.insert_pending(&info).expect("insert_pending");

        let mut fixups = make_fixup_cache();
        fixups.put(
            4242,
            vec![(
                "retro-call-4".to_string(),
                std::time::Instant::now(),
                Some("cosh".to_string()),
            )],
        );

        let mapper = make_mapper_with_pid(4242);
        apply_retro_session_fixup(
            &mut fixups,
            &mapper,
            &store,
            None,
            4242,
            Some("other-agent"),
        );

        let rows = store.list_pending_for_pids(&[4242]).expect("list pending");
        assert_eq!(
            rows[0].1.as_deref(),
            Some("0123456789abcdef0123456789abcdef"),
            "entries registered under another agent must not be repaired"
        );
        assert!(
            fixups.peek(&4242).is_none(),
            "reused-pid entries are dropped, not retried"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Build a fake `/proc/<pid>/status` file with a single `PPid:\t<ppid>` line.
    fn write_fake_status(proc_root: &Path, pid: u32, ppid: u32) {
        let dir = proc_root.join(pid.to_string());
        std::fs::create_dir_all(&dir).expect("create fake proc dir");
        std::fs::write(dir.join("status"), format!("Name:\tfake\nPPid:\t{ppid}\n"))
            .expect("write fake status");
    }

    #[test]
    fn collect_descendants_qwencode_wrapper_chain() {
        // cosh-shell(1) -> bash(2) -> node wrapper(3) -> node-22(4) -> node-22(5)
        // Asking for descendants of 3 must return {3, 4, 5} and ignore the
        // ancestor (1, 2) and unrelated branches (9, parented by 1).
        let dir = unique_tmp_dir("qwencode-chain");
        write_fake_status(&dir, 1, 0);
        write_fake_status(&dir, 2, 1);
        write_fake_status(&dir, 3, 2);
        write_fake_status(&dir, 4, 3);
        write_fake_status(&dir, 5, 4);
        write_fake_status(&dir, 9, 1);

        let got = AgentSight::collect_descendant_pids_impl(3, &dir);

        let mut want = HashSet::new();
        want.extend([3, 4, 5]);
        assert_eq!(got, want);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_descendants_root_only_when_no_children() {
        let dir = unique_tmp_dir("lone-root");
        write_fake_status(&dir, 7, 0);
        write_fake_status(&dir, 8, 0); // sibling, not a descendant

        let got = AgentSight::collect_descendant_pids_impl(7, &dir);
        let mut want = HashSet::new();
        want.insert(7);
        assert_eq!(got, want);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_descendants_returns_root_when_proc_missing() {
        let dir = unique_tmp_dir("missing-proc").join("does-not-exist");
        // Don't create `dir` — read_dir must fail and the helper must fall
        // back to returning {root}.
        let got = AgentSight::collect_descendant_pids_impl(42, &dir);
        let mut want = HashSet::new();
        want.insert(42);
        assert_eq!(got, want);
    }

    #[test]
    fn collect_descendants_skips_orphan_status_files() {
        // pid=10 has a status file; pid=11 has its directory but no status
        // file (process exited mid-walk). The walker should include 10 as a
        // descendant of 1 but silently skip 11.
        let dir = unique_tmp_dir("orphan-status");
        write_fake_status(&dir, 1, 0);
        write_fake_status(&dir, 10, 1);
        let orphan_dir = dir.join("11");
        std::fs::create_dir_all(&orphan_dir).expect("create orphan proc dir");

        let got = AgentSight::collect_descendant_pids_impl(1, &dir);
        let mut want = HashSet::new();
        want.extend([1, 10]);
        assert_eq!(got, want);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_descendants_ignores_non_numeric_entries() {
        // `/proc` always contains `self`, `thread-self`, `sys`, etc. The
        // walker must skip non-numeric entries without panicking.
        let dir = unique_tmp_dir("non-numeric");
        write_fake_status(&dir, 1, 0);
        write_fake_status(&dir, 2, 1);
        std::fs::create_dir_all(dir.join("self")).expect("create self dir");
        std::fs::create_dir_all(dir.join("thread-self")).expect("create thread-self dir");
        std::fs::write(dir.join("meminfo"), "ignored").expect("write stray file");

        let got = AgentSight::collect_descendant_pids_impl(1, &dir);
        let mut want = HashSet::new();
        want.extend([1, 2]);
        assert_eq!(got, want);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_register_descendants_only_qwencode() {
        // Positive case: the only agent currently opted in.
        assert!(AgentSight::should_register_descendants("QwenCode"));

        // Negative cases: every other known agent name must stay out so the
        // traced_processes BPF map stays lean.
        for name in [
            "CoshNG",
            "Claude",
            "Codex",
            "OpenClaw",
            "os-copilot",
            "cosh",
            "node-22",
            "Hermes",
            "Runloop",
            "CoshNG-shell",
            "",
            "qwencode", // lowercase must not match
            "qwenCode", // case-sensitive
        ] {
            assert!(
                !AgentSight::should_register_descendants(name),
                "descendant registration must be opt-in; {name:?} is not opted in"
            );
        }
    }
}
