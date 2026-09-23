//! Background health check loop
//!
//! Periodically scans for agent processes, detects their listening ports,
//! and probes them via HTTP to determine health status.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use super::port_detector::detect_listening_ports;
use super::store::{AgentHealthState, AgentHealthStatus, AgentRole, HealthStore, now_ms};
use crate::discovery::AgentScanner;
use crate::interruption::{
    InterruptionEvent, InterruptionType, ProcessExitStatus, is_reap_worker_agent,
    was_pid_oom_killed,
};
use crate::storage::sqlite::{GenAISqliteStore, InterruptionStore};

/// Infer the UI role for a discovered agent.
///
/// Rules (in order):
///   1. ports != empty            → Gateway (real service with TCP port)
///   2. agent has no service port → Client (stand-alone CLI like Codex
///                                  whose UI card adds no value when
///                                  there is no gateway endpoint)
///   3. parent is same agent_name → Worker (genuine fork, fold under parent)
///   4. otherwise                 → Gateway (independent process, own card)
///
/// Two separately-launched hermes/openclaw client instances are
/// independent (no parent-child link, different terminals), so they
/// each deserve their own primary card; only true forks go into the
/// associated-processes drawer of their parent.
fn infer_agent_role(
    agent_name: &str,
    ports: &[u16],
    ppid: Option<u32>,
    agent_name_by_pid: &HashMap<u32, String>,
) -> AgentRole {
    if !ports.is_empty() {
        return AgentRole::Gateway;
    }
    if agent_name == "Codex" {
        return AgentRole::Client;
    }
    if let Some(pp) = ppid {
        if agent_name_by_pid
            .get(&pp)
            .map(|n| n == agent_name)
            .unwrap_or(false)
        {
            return AgentRole::Worker;
        }
    }
    AgentRole::Gateway
}

fn read_workspace_path(pid: u32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
}

/// Background health checker that periodically probes discovered agents
pub struct HealthChecker {
    store: Arc<RwLock<HealthStore>>,
    interval: Duration,
    http_timeout: Duration,
    /// Optional interruption store for writing agent_crash events
    interruption_store: Option<Arc<InterruptionStore>>,
    /// Optional GenAI store for querying pending calls and marking them interrupted
    genai_store: Option<Arc<GenAISqliteStore>>,
}

impl HealthChecker {
    /// Create a new HealthChecker.
    ///
    /// # Arguments
    /// * `store` - Shared health store for writing results
    /// * `interval` - Time between health check cycles
    pub fn new(store: Arc<RwLock<HealthStore>>, interval: Duration) -> Self {
        Self {
            store,
            interval,
            http_timeout: Duration::from_secs(5),
            interruption_store: None,
            genai_store: None,
        }
    }

    /// Create with an interruption store so offline events trigger `agent_crash`.
    pub fn with_interruption_store(mut self, interruption_store: Arc<InterruptionStore>) -> Self {
        self.interruption_store = Some(interruption_store);
        self
    }

    /// Create with a GenAI store to query pending calls on agent crash.
    pub fn with_genai_store(mut self, genai_store: Arc<GenAISqliteStore>) -> Self {
        self.genai_store = Some(genai_store);
        self
    }

    /// Spawn the health check loop on a background thread.
    ///
    /// Returns the `JoinHandle` — the thread runs until the process exits.
    pub fn start(self) -> thread::JoinHandle<()> {
        thread::spawn(move || self.run())
    }

    /// Main health check loop (blocking).
    fn run(self) {
        log::info!(
            "Health checker started: interval={}s, http_timeout={}s",
            self.interval.as_secs(),
            self.http_timeout.as_secs(),
        );

        // Do an initial check immediately, then loop with interval
        loop {
            self.check_once();
            thread::sleep(self.interval);
        }
    }

    /// Perform a single health check cycle for all discovered agents.
    fn check_once(&self) {
        let mut scanner = AgentScanner::from_rules(&crate::config::default_cmdline_rules(), &[]);
        let agents = scanner.scan();

        let active_pids: HashSet<u32> = agents.iter().map(|a| a.pid).collect();

        // Mark gone processes as Offline (instead of deleting immediately)
        let newly_offline = if let Ok(mut store) = self.store.write() {
            store.last_scan_time = now_ms();
            let offline = store.mark_stale_offline(&active_pids);
            // 自动清理超过 5 分钟的 Offline 条目，避免历史 PID 在 Sidebar 长期残留
            const OFFLINE_TTL_MS: u64 = 5 * 60 * 1000;
            let removed = store.cleanup_stale_offline(OFFLINE_TTL_MS);
            if removed > 0 {
                log::info!(
                    "HealthStore: cleaned {} stale offline entries (TTL={}s)",
                    removed,
                    OFFLINE_TTL_MS / 1000
                );
            }
            offline
        } else {
            vec![]
        };

        self.record_offline_agent_crashes(&newly_offline);

        log::debug!("Health check: found {} agent(s)", agents.len());

        // Collect agent names by PID for role inference (detect parent-child within same agent)
        let agent_name_by_pid: HashMap<u32, String> = agents
            .iter()
            .map(|a| (a.pid, a.agent_info.name.clone()))
            .collect();

        // Pre-scan listening ports per pid (also avoids scanning /proc twice).
        let ports_by_pid: HashMap<u32, Vec<u16>> = agents
            .iter()
            .map(|a| (a.pid, detect_listening_ports(a.pid)))
            .collect();

        for agent in &agents {
            let workspace_path = read_workspace_path(agent.pid);
            let ports = ports_by_pid.get(&agent.pid).cloned().unwrap_or_default();
            // Cosh has no daemon process and does not support keepalive/restart.
            // Build restart_cmd only for agents that support it.
            let restart_cmd = if agent.agent_info.name == "Cosh" {
                None
            } else {
                Some(build_restart_cmd(&agent.exe_path, &agent.cmdline_args))
            };

            // Read parent PID from /proc/<pid>/stat for role inference
            let ppid = read_ppid(agent.pid);

            let role = infer_agent_role(&agent.agent_info.name, &ports, ppid, &agent_name_by_pid);

            let status = if ports.is_empty() {
                AgentHealthStatus {
                    pid: agent.pid,
                    agent_name: agent.agent_info.name.clone(),
                    category: agent.agent_info.category.clone(),
                    exe_path: agent.exe_path.clone(),
                    workspace_path,
                    ports: vec![],
                    status: AgentHealthState::NoPort,
                    last_check_time: now_ms(),
                    latency_ms: None,
                    error_message: None,
                    restart_cmd,
                    offline_since: None,
                    role,
                    parent_pid: ppid,
                    has_crash: false,
                }
            } else {
                self.probe_agent(agent, &ports, restart_cmd, role, ppid)
            };

            if let Ok(mut store) = self.store.write() {
                store.update(agent.pid, status);
            }
        }
    }

    /// Write agent_crash interruption events for processes that just went offline.
    ///
    /// Deduplication strategy:
    ///   - Cosh spawns 2 node processes per session (parent + worker). LLM traffic
    ///     may be recorded under either pid. We group all pids for the same agent_name
    ///     and query them together so we see all calls in one pass.
    ///   - OpenClaw is a single-pid gateway that may serve multiple concurrent sessions.
    ///     All sessions for that pid are returned and each gets its own event.
    ///   - Deduplication key is (agent_name, session_id, conversation_id): at most one
    ///     agent_crash event is written per unique (agent, session, conversation) triple.
    ///
    /// Split out of `check_once` so the crash-vs-normal-shutdown decision can
    /// be unit-tested by injecting offline entries without scanning /proc.
    fn record_offline_agent_crashes(&self, newly_offline: &[AgentHealthStatus]) {
        if !newly_offline.is_empty() {
            if let Some(ref istore) = self.interruption_store {
                let now_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);

                // Group newly-offline entries by agent_name so multi-process agents
                // (e.g. Cosh) are processed together in a single DB query.
                let mut by_agent: HashMap<String, Vec<&AgentHealthStatus>> = HashMap::new();
                for offline in newly_offline {
                    by_agent
                        .entry(offline.agent_name.clone())
                        .or_default()
                        .push(offline);
                }

                // Dedup key: (agent_name, session_id, conversation_id)
                let mut seen_conv: HashSet<(String, Option<String>, Option<String>)> =
                    HashSet::new();

                // Track PIDs that had crash events (for sidebar display filtering)
                let mut crash_pids: Vec<u32> = Vec::new();

                for (agent_name, group) in &by_agent {
                    let pids: Vec<i32> = group.iter().map(|o| o.pid as i32).collect();
                    // Use the representative offline entry for metadata (pid shown in detail).
                    // When multiple pids exist (Cosh), use the largest pid (worker) as it is
                    // most likely the one that carried LLM traffic.
                    let rep = group.iter().max_by_key(|o| o.pid).unwrap();

                    // ── Branch A: pending (in-flight) LLM calls ──────────────────────────
                    let pending_calls = self.get_pending_calls_for_pids(&pids);
                    if !pending_calls.is_empty() {
                        // Trace mode records each agent's raw exit status into
                        // the shared interruption DB. When every offline pid in
                        // this group is a trace-confirmed clean exit, this is a
                        // single-shot agent shutting down normally (issue
                        // #1989): trace deliberately wrote no agent_crash, so
                        // this backup path must not re-add one. Missing records
                        // keep the historical fallback (offline + pending →
                        // crash) for exits trace did not observe.
                        let exit_status_by_pid: HashMap<i32, ProcessExitStatus> = pids
                            .iter()
                            .filter_map(|&p| {
                                istore
                                    .get_process_exit_recent(p, 120)
                                    .map(|raw| (p, ProcessExitStatus::decode(raw)))
                            })
                            .collect();
                        let all_clean_exit = exit_status_by_pid.len() == pids.len()
                            && exit_status_by_pid.values().all(|s| {
                                s.is_clean()
                                    || (s.is_graceful_reap() && is_reap_worker_agent(agent_name))
                            });
                        if all_clean_exit {
                            log::debug!(
                                "Agent {agent_name} (pids={pids:?}) exited cleanly (trace-recorded exit status) — treating as normal shutdown despite {} pending call(s)",
                                pending_calls.len()
                            );
                            continue;
                        }

                        // Check if any of the crashed PIDs were OOM-killed
                        let is_oom = pids.iter().any(|&p| was_pid_oom_killed(p));
                        if is_oom {
                            log::info!(
                                "Agent {agent_name} (pids={pids:?}) was OOM-killed (confirmed via dmesg)"
                            );
                        }

                        let mut by_conv: HashMap<
                            (Option<String>, Option<String>),
                            Vec<(String, Option<String>)>,
                        > = HashMap::new();
                        for (call_id, session_id, _trace_id, conversation_id) in &pending_calls {
                            by_conv
                                .entry((session_id.clone(), conversation_id.clone()))
                                .or_default()
                                .push((call_id.clone(), session_id.clone()));
                        }
                        for ((session_id, conversation_id), calls) in &by_conv {
                            let dedup_key = (
                                agent_name.clone(),
                                session_id.clone(),
                                conversation_id.clone(),
                            );
                            if !seen_conv.insert(dedup_key) {
                                log::debug!(
                                    "Skipping duplicate agent_crash for {agent_name} session={session_id:?} conversation={conversation_id:?}"
                                );
                                continue;
                            }
                            // Dedup: skip if trace-mode already recorded a
                            // recent agent_crash for this PID (within 120s).
                            if istore.agent_crash_exists_recent(rep.pid as i32, 120) {
                                log::debug!(
                                    "Skipping agent_crash for pid={} — already recorded by trace mode",
                                    rep.pid,
                                );
                                continue;
                            }
                            let call_ids: Vec<&str> =
                                calls.iter().map(|(c, _)| c.as_str()).collect();
                            let mut detail = serde_json::json!({
                                "pid": rep.pid,
                                "agent_name": agent_name,
                                "exe_path": rep.exe_path.clone(),
                                "call_ids": call_ids,
                            });
                            if is_oom {
                                detail["oom"] = serde_json::json!(true);
                                detail["source"] = serde_json::json!("healthchecker+dmesg");
                            }
                            // Attach the trace-recorded exit status when
                            // available; omitted (not fabricated) when trace
                            // did not observe this pid's exit.
                            if let Some(status) = exit_status_by_pid.get(&(rep.pid as i32)) {
                                detail["exit_code"] = serde_json::json!(status.code);
                                detail["signal"] = serde_json::json!(status.signal);
                                detail["core_dump"] = serde_json::json!(status.core_dump);
                            }
                            let event = InterruptionEvent::new(
                                InterruptionType::AgentCrash,
                                session_id.clone(),
                                None,
                                conversation_id.clone(),
                                None,
                                Some(rep.pid as i32),
                                Some(agent_name.clone()),
                                now_ns,
                                Some(detail),
                            );
                            if let Err(e) = istore.insert(&event) {
                                log::warn!(
                                    "Failed to record agent_crash for pid={}: {}",
                                    rep.pid,
                                    e
                                );
                            } else {
                                log::info!(
                                    "Recorded agent_crash for {} (pid={}, session={:?}, conversation={:?}, {} call(s), oom={})",
                                    agent_name,
                                    rep.pid,
                                    session_id,
                                    conversation_id,
                                    calls.len(),
                                    is_oom
                                );
                            }
                        }
                        for o in group {
                            self.mark_pending_interrupted(o.pid, "agent_crash");
                        }
                        // Mark all PIDs in this group as having a crash event
                        crash_pids.extend(group.iter().map(|o| o.pid));
                    } else {
                        // No pending calls — treat as normal/graceful shutdown.
                        log::debug!(
                            "Agent {agent_name} (pids={pids:?}) exited with no pending calls — treating as normal shutdown"
                        );
                    }
                }

                // Update store: mark crash PIDs, then remove normal exits
                if let Ok(mut store) = self.store.write() {
                    for pid in &crash_pids {
                        store.mark_has_crash(*pid);
                    }
                    let removed = store.remove_normal_exits();
                    if removed > 0 {
                        log::debug!(
                            "Removed {} normal-exit entries from health store (no crash event)",
                            removed
                        );
                    }
                }
            }
        }
    }

    /// Probe an agent's ports via HTTP and return its health status.
    ///
    /// Tries each port in order.
    /// - 2xx/3xx/4xx/5xx response         → Healthy
    /// - Read timeout (process is hung)   → Hung
    /// - Connection refused / unreachable → Unhealthy
    fn probe_agent(
        &self,
        agent: &crate::discovery::DiscoveredAgent,
        ports: &[u16],
        restart_cmd: Option<Vec<String>>,
        role: AgentRole,
        parent_pid: Option<u32>,
    ) -> AgentHealthStatus {
        let mut last_error = String::new();
        let workspace_path = read_workspace_path(agent.pid);
        // 标记是否遇到了超时错误（区分 hung vs unreachable）
        let mut timed_out = false;

        for &port in ports {
            let url = format!("http://127.0.0.1:{port}/");
            let start = Instant::now();

            let result = ureq::AgentBuilder::new()
                .timeout_connect(self.http_timeout)
                .timeout_read(self.http_timeout)
                .build()
                .get(&url)
                .call();

            let latency = start.elapsed().as_millis() as u64;

            match result {
                Ok(_) => {
                    return AgentHealthStatus {
                        pid: agent.pid,
                        agent_name: agent.agent_info.name.clone(),
                        category: agent.agent_info.category.clone(),
                        exe_path: agent.exe_path.clone(),
                        workspace_path: workspace_path.clone(),
                        ports: ports.to_vec(),
                        status: AgentHealthState::Healthy,
                        last_check_time: now_ms(),
                        latency_ms: Some(latency),
                        error_message: None,
                        restart_cmd,
                        offline_since: None,
                        role: role.clone(),
                        parent_pid,
                        has_crash: false,
                    };
                }
                Err(ureq::Error::Status(_code, _resp)) => {
                    // 非 2xx 响应 — 进程仍在响应
                    return AgentHealthStatus {
                        pid: agent.pid,
                        agent_name: agent.agent_info.name.clone(),
                        category: agent.agent_info.category.clone(),
                        exe_path: agent.exe_path.clone(),
                        workspace_path: workspace_path.clone(),
                        ports: ports.to_vec(),
                        status: AgentHealthState::Healthy,
                        last_check_time: now_ms(),
                        latency_ms: Some(latency),
                        error_message: None,
                        restart_cmd,
                        offline_since: None,
                        role: role.clone(),
                        parent_pid,
                        has_crash: false,
                    };
                }
                Err(ureq::Error::Transport(e)) => {
                    let msg = e.to_string();
                    // ureq 的读超时 / 写超时消息均包含 "timed out"
                    if msg.to_lowercase().contains("timed out") {
                        timed_out = true;
                        last_error = format!("响应超时 ({latency}ms): {msg}");
                    } else {
                        last_error = msg.clone();
                    }
                    log::debug!(
                        "Health probe failed for {} (pid={}) on port {}: {}",
                        agent.agent_info.name,
                        agent.pid,
                        port,
                        msg
                    );
                }
            }
        }

        // 所有端口均失败 — 根据错误类型决定状态
        let status = if timed_out {
            AgentHealthState::Hung
        } else {
            AgentHealthState::Unhealthy
        };

        AgentHealthStatus {
            pid: agent.pid,
            agent_name: agent.agent_info.name.clone(),
            category: agent.agent_info.category.clone(),
            exe_path: agent.exe_path.clone(),
            workspace_path,
            ports: ports.to_vec(),
            status,
            last_check_time: now_ms(),
            latency_ms: None,
            error_message: Some(last_error),
            restart_cmd,
            offline_since: None,
            role,
            parent_pid,
            has_crash: false,
        }
    }

    /// Query pending LLM calls for multiple PIDs at once.
    ///
    /// Returns a list of (call_id, session_id, trace_id, conversation_id) tuples.
    fn get_pending_calls_for_pids(
        &self,
        pids: &[i32],
    ) -> Vec<(String, Option<String>, Option<String>, Option<String>)> {
        if let Some(ref genai_store) = self.genai_store {
            match genai_store.list_pending_for_pids(pids) {
                Ok(calls) => calls,
                Err(e) => {
                    log::warn!("Failed to query pending calls for pids={pids:?}: {e}");
                    vec![]
                }
            }
        } else {
            vec![]
        }
    }

    /// Mark all pending calls for a PID as interrupted in genai_events.
    fn mark_pending_interrupted(&self, pid: u32, itype: &str) {
        if let Some(ref genai_store) = self.genai_store {
            if let Err(e) = genai_store.mark_pending_interrupted_for_pid(pid as i32, itype) {
                log::warn!("Failed to mark pending calls as interrupted for pid={pid}: {e}");
            }
        }
    }
}

/// 构造重启命令向量：[exe, arg1, arg2, ...]
///
/// 过滤掉空字符串，保证命令有效。
fn build_restart_cmd(exe_path: &str, cmdline_args: &[String]) -> Vec<String> {
    let mut cmd = vec![exe_path.to_string()];
    // cmdline_args[0] 通常是 exe 本身（argv[0]），跳过以避免重复
    let args: Vec<_> = cmdline_args
        .iter()
        .skip(1)
        .filter(|a| !a.is_empty())
        .cloned()
        .collect();
    cmd.extend(args);
    cmd
}

/// Read the parent PID (ppid) from `<procfs root>/<pid>/stat`.
/// Returns None if the file cannot be read or parsed.
fn read_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(crate::utils::procfs::proc_pid_entry(pid, "stat")).ok()?;
    // Format: "pid (comm) state ppid ..."
    // Find the closing ')' first (comm may contain spaces/parens)
    let after_comm = stat.rsplit_once(')')?.1;
    // after_comm = " state ppid ..."
    let mut fields = after_comm.split_whitespace();
    let _state = fields.next()?;
    let ppid_str = fields.next()?;
    ppid_str.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_ppid_current_process() {
        // Whatever the parent is, our own stat entry parses.
        assert!(read_ppid(std::process::id()).is_some());
    }

    #[test]
    fn test_infer_agent_role_gateway_with_ports() {
        let mut map = HashMap::new();
        map.insert(1, "Codex".to_string());
        assert_eq!(
            infer_agent_role("Codex", &[8080], None, &map),
            AgentRole::Gateway
        );
    }

    #[test]
    fn test_infer_agent_role_codex_client_without_ports() {
        let map = HashMap::new();
        assert_eq!(
            infer_agent_role("Codex", &[], None, &map),
            AgentRole::Client
        );
    }

    #[test]
    fn test_infer_agent_role_worker_same_parent_name() {
        let mut map = HashMap::new();
        map.insert(1, "Hermes".to_string());
        assert_eq!(
            infer_agent_role("Hermes", &[], Some(1), &map),
            AgentRole::Worker
        );
    }

    #[test]
    fn test_infer_agent_role_gateway_different_parent_name() {
        let mut map = HashMap::new();
        map.insert(1, "Other".to_string());
        assert_eq!(
            infer_agent_role("Hermes", &[], Some(1), &map),
            AgentRole::Gateway
        );
    }

    #[test]
    fn test_infer_agent_role_gateway_no_parent() {
        let map = HashMap::new();
        assert_eq!(
            infer_agent_role("Hermes", &[], None, &map),
            AgentRole::Gateway
        );
    }

    // ── Tests for record_offline_agent_crashes (issue #1989 backup path) ──

    fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("agentsight-hc-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Build a checker wired to fresh temp stores plus one pending call for
    /// `pid`, mirroring the serve-mode setup in `run_server`.
    fn setup_checker(
        tag: &str,
        pid: i32,
    ) -> (
        std::path::PathBuf,
        HealthChecker,
        Arc<GenAISqliteStore>,
        Arc<InterruptionStore>,
    ) {
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
        let info = crate::storage::sqlite::genai::PendingCallInfo {
            call_id: format!("hc-call-{tag}"),
            trace_id: None,
            conversation_id: None,
            session_id: None,
            start_timestamp_ns: 1_000_000_000,
            pid,
            process_name: "test".to_string(),
            agent_name: Some("cosh-core".to_string()),
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
        };
        genai_store.insert_pending(&info).expect("insert_pending");
        let checker = HealthChecker::new(
            Arc::new(RwLock::new(HealthStore::new())),
            Duration::from_secs(30),
        )
        .with_interruption_store(Arc::clone(&istore))
        .with_genai_store(Arc::clone(&genai_store));
        (dir, checker, genai_store, istore)
    }

    fn offline_status(pid: u32) -> AgentHealthStatus {
        AgentHealthStatus {
            pid,
            agent_name: "CoshNG".to_string(),
            category: "cli".to_string(),
            exe_path: "/usr/bin/cosh-core".to_string(),
            workspace_path: None,
            ports: vec![],
            status: AgentHealthState::Offline,
            last_check_time: now_ms(),
            latency_ms: None,
            error_message: None,
            restart_cmd: None,
            offline_since: Some(now_ms()),
            role: AgentRole::Client,
            parent_pid: None,
            has_crash: false,
        }
    }

    #[test]
    fn reports_current_process_workspace() {
        let expected = std::env::current_dir()
            .expect("current directory")
            .canonicalize()
            .expect("canonical current directory");
        assert_eq!(
            read_workspace_path(std::process::id()).as_deref(),
            expected.to_str()
        );
    }

    fn list_crash_events(
        istore: &InterruptionStore,
    ) -> Vec<crate::storage::sqlite::interruption::InterruptionRecord> {
        istore
            .list(0, i64::MAX, None, Some("agent_crash"), None, None, 100)
            .expect("list interruptions")
    }

    #[test]
    fn test_offline_with_pending_and_clean_exit_record_skips_agent_crash() {
        // PID large enough to never collide with dmesg OOM records.
        let pid = 4_100_001;
        let (dir, checker, genai_store, istore) = setup_checker("clean", pid);

        // trace mode recorded a clean exit (exit 0 → raw 0x0) for this pid
        istore.record_process_exit(pid, 0x0).expect("record exit");

        checker.record_offline_agent_crashes(&[offline_status(pid as u32)]);

        assert!(
            list_crash_events(&istore).is_empty(),
            "clean exit recorded by trace must not be re-reported as agent_crash"
        );
        // Pending call must stay pending: marking it interrupted would pollute
        // grader root_cause attribution just like the crash event itself.
        assert_eq!(
            genai_store
                .list_pending_for_pids(&[pid])
                .expect("list pending")
                .len(),
            1,
            "pending call must not be marked interrupted on clean exit"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_offline_with_pending_and_sigterm_reap_record_skips_agent_crash() {
        let pid = 4_100_004;
        let (dir, checker, genai_store, istore) = setup_checker("sigterm", pid);

        // Trace mode recorded a graceful SIGTERM reap (raw 0x0f) for this pid:
        // the checker backup path must not re-report it as a crash.
        istore.record_process_exit(pid, 0x0f).expect("record exit");

        checker.record_offline_agent_crashes(&[offline_status(pid as u32)]);

        assert!(
            list_crash_events(&istore).is_empty(),
            "SIGTERM reap recorded by trace must not be re-reported as agent_crash"
        );
        assert_eq!(
            genai_store
                .list_pending_for_pids(&[pid])
                .expect("list pending")
                .len(),
            1,
            "pending call must not be marked interrupted on graceful reap"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_offline_with_pending_and_sigterm_non_worker_still_records_agent_crash() {
        let pid = 4_100_005;
        let (dir, checker, _genai_store, istore) = setup_checker("sigterm-nonworker", pid);

        // A non-worker agent (Codex) interrupted by SIGTERM is not eligible for
        // the graceful-reap exemption; the checker backup path must record it.
        istore.record_process_exit(pid, 0x0f).expect("record exit");
        let mut status = offline_status(pid as u32);
        status.agent_name = "Codex".to_string();

        checker.record_offline_agent_crashes(&[status]);

        assert_eq!(
            list_crash_events(&istore).len(),
            1,
            "SIGTERM on a non-worker agent must still be recorded as agent_crash"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_offline_with_pending_and_no_exit_record_still_records_agent_crash() {
        let pid = 4_100_002;
        let (dir, checker, genai_store, istore) = setup_checker("norec", pid);

        // No process_exits record: trace did not observe this exit, so the
        // historical fallback (offline + pending → crash) must be preserved.
        checker.record_offline_agent_crashes(&[offline_status(pid as u32)]);

        let events = list_crash_events(&istore);
        assert_eq!(events.len(), 1, "fallback path must still record the crash");
        let detail: serde_json::Value =
            serde_json::from_str(events[0].detail.as_deref().expect("detail json"))
                .expect("parse detail");
        assert_eq!(detail["pid"], pid);
        // Exit status is unknown here — fields must be omitted, not fabricated.
        assert!(detail.get("exit_code").is_none());
        assert!(
            genai_store
                .list_pending_for_pids(&[pid])
                .expect("list pending")
                .is_empty(),
            "fallback path must mark the pending call as interrupted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_offline_with_pending_and_sigkill_exit_record_records_crash_with_detail() {
        let pid = 4_100_003;
        let (dir, checker, genai_store, istore) = setup_checker("sigkill", pid);

        // SIGKILL → raw 0x9 (measured on a real kernel): not a clean exit,
        // so the checker must behave exactly as before the #1989 fix.
        istore.record_process_exit(pid, 0x9).expect("record exit");

        checker.record_offline_agent_crashes(&[offline_status(pid as u32)]);

        let events = list_crash_events(&istore);
        assert_eq!(events.len(), 1, "SIGKILL must still be recorded as crash");
        let detail: serde_json::Value =
            serde_json::from_str(events[0].detail.as_deref().expect("detail json"))
                .expect("parse detail");
        assert_eq!(detail["signal"], 9);
        assert_eq!(detail["exit_code"], 0);
        assert_eq!(detail["core_dump"], false);
        assert!(
            genai_store
                .list_pending_for_pids(&[pid])
                .expect("list pending")
                .is_empty(),
            "crash path must mark the pending call as interrupted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
