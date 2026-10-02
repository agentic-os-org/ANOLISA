//! Shared health state for agent health check results
//!
//! This module provides the `HealthStore` type that holds the latest health
//! status of all discovered agent processes, shared between the background
//! health checker thread and the API handler via `Arc<RwLock<...>>`.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Health state of an agent process
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentHealthState {
    /// HTTP probe returned a response (any status code)
    Healthy,
    /// HTTP probe failed (connection refused, etc.) — port unreachable
    Unhealthy,
    /// HTTP probe connected but timed out waiting for a response — process is hung
    Hung,
    /// Not yet checked
    Unknown,
    /// Process is alive but has no listening TCP port
    NoPort,
    /// Process has exited — kept for user acknowledgement
    Offline,
}

/// Role of an agent process in the process group
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Service process with listening TCP ports (e.g. OpenClaw Gateway on 18789)
    Gateway,
    /// Client process without ports (e.g. TUI main process)
    Client,
    /// Worker sub-process forked from a Client (parent_pid is also same agent)
    Worker,
}

/// Health status of a single agent process
#[derive(Debug, Clone, Serialize)]
pub struct AgentHealthStatus {
    pub pid: u32,
    pub agent_name: String,
    pub category: String,
    pub exe_path: String,
    /// Process working directory, used as the default protection scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    /// Detected listening TCP ports
    pub ports: Vec<u16>,
    pub status: AgentHealthState,
    /// Unix timestamp in milliseconds of the last check
    pub last_check_time: u64,
    /// HTTP response latency in milliseconds (if healthy)
    pub latency_ms: Option<u64>,
    /// Error description (if unhealthy)
    pub error_message: Option<String>,
    /// 用于重启的完整命令行（exe + args），None 表示不支持重启
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_cmd: Option<Vec<String>>,
    /// 进入 Offline 状态的时刻（Unix ms）。仅 Offline 项有值，用于 TTL 自动清理。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offline_since: Option<u64>,
    /// 进程角色：Gateway（有端口）/ Client（无端口）/ Worker（子进程）
    pub role: AgentRole,
    /// 父进程 PID（用于折叠展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_pid: Option<u32>,
    /// 进程退出时是否关联了 agent_crash 中断事件（有未完成的 LLM 调用）。
    /// 仅 Offline 状态有意义：true = 异常退出（影响了进行中的对话），false = 正常退出。
    #[serde(default)]
    pub has_crash: bool,
}

/// Stores the latest health check results for all tracked agents
pub struct HealthStore {
    agents: HashMap<u32, AgentHealthStatus>,
    /// Unix timestamp in milliseconds of the last full scan
    pub last_scan_time: u64,
}

impl Default for HealthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthStore {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            last_scan_time: 0,
        }
    }

    /// Update or insert the health status for a given PID
    pub fn update(&mut self, pid: u32, status: AgentHealthStatus) {
        self.agents.insert(pid, status);
    }

    /// Mark stale PIDs as Offline instead of removing them outright.
    /// Already-offline entries are left untouched (preserve offline_since time).
    pub fn mark_stale_offline(&mut self, active_pids: &HashSet<u32>) -> Vec<AgentHealthStatus> {
        let mut newly_offline = Vec::new();
        for (pid, entry) in self.agents.iter_mut() {
            if !active_pids.contains(pid) && entry.status != AgentHealthState::Offline {
                entry.status = AgentHealthState::Offline;
                entry.last_check_time = now_ms();
                entry.latency_ms = None;
                entry.error_message = Some("进程已退出".to_string());
                entry.offline_since = Some(now_ms());
                entry.has_crash = false;
                newly_offline.push(entry.clone());
            }
        }
        newly_offline
    }

    /// 标记指定 PID 的 offline 条目为关联了 crash 事件（异常退出）。
    pub fn mark_has_crash(&mut self, pid: u32) {
        if let Some(entry) = self.agents.get_mut(&pid) {
            entry.has_crash = true;
        }
    }

    /// 移除所有无 crash 关联的 offline 条目（正常退出不需要展示）。
    /// 返回被移除的 PID 数量。
    pub fn remove_normal_exits(&mut self) -> usize {
        let before = self.agents.len();
        self.agents
            .retain(|_, entry| entry.status != AgentHealthState::Offline || entry.has_crash);
        before - self.agents.len()
    }

    /// 自动清理超过 TTL 的 Offline 条目（避免历史进程长期残留 UI）。
    /// `ttl_ms`: Offline 状态保留时长，超过则从 store 移除。
    /// 返回被移除的 PID 数量。
    pub fn cleanup_stale_offline(&mut self, ttl_ms: u64) -> usize {
        let now = now_ms();
        let before = self.agents.len();
        self.agents.retain(|_, entry| {
            if entry.status != AgentHealthState::Offline {
                return true;
            }
            match entry.offline_since {
                Some(since) => now.saturating_sub(since) < ttl_ms,
                None => true, // 兼容老数据：没有时间戳的暂不清理
            }
        });
        before - self.agents.len()
    }

    /// Remove a specific PID (user-acknowledged deletion)
    pub fn remove_by_pid(&mut self, pid: u32) -> bool {
        self.agents.remove(&pid).is_some()
    }

    /// Return a snapshot of all agent health statuses
    pub fn all_agents(&self) -> Vec<AgentHealthStatus> {
        self.agents.values().cloned().collect()
    }
}

/// Current time in Unix milliseconds
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(pid: u32, status: AgentHealthState, has_crash: bool) -> AgentHealthStatus {
        AgentHealthStatus {
            pid,
            agent_name: format!("agent-{pid}"),
            category: "test".to_string(),
            exe_path: "/usr/bin/test".to_string(),
            workspace_path: None,
            ports: vec![],
            status,
            last_check_time: now_ms(),
            latency_ms: None,
            error_message: None,
            restart_cmd: None,
            offline_since: None,
            role: AgentRole::Gateway,
            parent_pid: None,
            has_crash,
        }
    }

    #[test]
    fn test_new_store_is_empty() {
        let store = HealthStore::new();
        assert_eq!(store.all_agents().len(), 0);
        assert_eq!(store.last_scan_time, 0);
    }

    #[test]
    fn test_update_and_retrieve() {
        let mut store = HealthStore::new();
        let agent = make_agent(100, AgentHealthState::Healthy, false);
        store.update(100, agent.clone());
        let all = store.all_agents();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].pid, 100);
    }

    #[test]
    fn test_mark_stale_offline() {
        let mut store = HealthStore::new();
        store.update(1, make_agent(1, AgentHealthState::Healthy, false));
        store.update(2, make_agent(2, AgentHealthState::Healthy, false));

        // PID 1 is still active, PID 2 is gone
        let active: HashSet<u32> = [1].into_iter().collect();
        let newly_offline = store.mark_stale_offline(&active);

        assert_eq!(newly_offline.len(), 1);
        assert_eq!(newly_offline[0].pid, 2);
        assert_eq!(newly_offline[0].status, AgentHealthState::Offline);
        assert!(!newly_offline[0].has_crash);
        assert!(newly_offline[0].offline_since.is_some());
    }

    #[test]
    fn test_mark_stale_offline_idempotent() {
        let mut store = HealthStore::new();
        store.update(1, make_agent(1, AgentHealthState::Healthy, false));

        let active: HashSet<u32> = HashSet::new();
        let first = store.mark_stale_offline(&active);
        assert_eq!(first.len(), 1);

        // Second call should not re-mark already-offline entries
        let second = store.mark_stale_offline(&active);
        assert_eq!(second.len(), 0);
    }

    #[test]
    fn test_mark_has_crash() {
        let mut store = HealthStore::new();
        let mut agent = make_agent(10, AgentHealthState::Offline, false);
        agent.offline_since = Some(now_ms());
        store.update(10, agent);

        assert!(!store.all_agents()[0].has_crash);

        store.mark_has_crash(10);
        assert!(store.all_agents()[0].has_crash);
    }

    #[test]
    fn test_mark_has_crash_nonexistent_pid() {
        let mut store = HealthStore::new();
        // Should not panic on missing PID
        store.mark_has_crash(999);
        assert_eq!(store.all_agents().len(), 0);
    }

    #[test]
    fn test_remove_normal_exits() {
        let mut store = HealthStore::new();
        // Crash exit (should be kept)
        store.update(1, make_agent(1, AgentHealthState::Offline, true));
        // Normal exit (should be removed)
        store.update(2, make_agent(2, AgentHealthState::Offline, false));
        // Still alive (should be kept)
        store.update(3, make_agent(3, AgentHealthState::Healthy, false));

        let removed = store.remove_normal_exits();
        assert_eq!(removed, 1);

        let remaining = store.all_agents();
        assert_eq!(remaining.len(), 2);
        let pids: HashSet<u32> = remaining.iter().map(|a| a.pid).collect();
        assert!(pids.contains(&1)); // crash kept
        assert!(pids.contains(&3)); // alive kept
        assert!(!pids.contains(&2)); // normal exit removed
    }

    #[test]
    fn test_remove_normal_exits_no_offline() {
        let mut store = HealthStore::new();
        store.update(1, make_agent(1, AgentHealthState::Healthy, false));
        store.update(2, make_agent(2, AgentHealthState::Hung, false));

        let removed = store.remove_normal_exits();
        assert_eq!(removed, 0);
        assert_eq!(store.all_agents().len(), 2);
    }

    #[test]
    fn test_cleanup_stale_offline() {
        let mut store = HealthStore::new();
        let mut agent = make_agent(1, AgentHealthState::Offline, true);
        // Set offline_since to 10 minutes ago
        agent.offline_since = Some(now_ms().saturating_sub(10 * 60 * 1000));
        store.update(1, agent);

        // TTL = 5 minutes -> should be cleaned
        let removed = store.cleanup_stale_offline(5 * 60 * 1000);
        assert_eq!(removed, 1);
        assert_eq!(store.all_agents().len(), 0);
    }

    #[test]
    fn test_cleanup_stale_offline_within_ttl() {
        let mut store = HealthStore::new();
        let mut agent = make_agent(1, AgentHealthState::Offline, true);
        agent.offline_since = Some(now_ms()); // just now
        store.update(1, agent);

        let removed = store.cleanup_stale_offline(5 * 60 * 1000);
        assert_eq!(removed, 0);
        assert_eq!(store.all_agents().len(), 1);
    }

    #[test]
    fn test_remove_by_pid() {
        let mut store = HealthStore::new();
        store.update(5, make_agent(5, AgentHealthState::Healthy, false));

        assert!(store.remove_by_pid(5));
        assert!(!store.remove_by_pid(5)); // already removed
        assert_eq!(store.all_agents().len(), 0);
    }

    #[test]
    fn test_default_trait() {
        let store = HealthStore::default();
        assert_eq!(store.all_agents().len(), 0);
        assert_eq!(store.last_scan_time, 0);
    }
}
