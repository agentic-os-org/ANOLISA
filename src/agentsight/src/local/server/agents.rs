//! Agent process discovery endpoint.
//!
//! Uses the cross-platform `sysinfo` crate to enumerate running processes
//! and match them against known AI coding agent signatures. Works on both
//! Linux and macOS without platform-specific code.
//!
//! Ported from agentopt's `crates/server/src/agents.rs`, adapted to actix-web.

use std::collections::HashMap;

use actix_web::{HttpResponse, Responder, get};
use serde::Serialize;
use sysinfo::System;

// ── Known agent signatures ────────────────────────────────────────────────

/// A known agent type with its process matching patterns and display info.
struct AgentSignature {
    /// Display name
    name: &'static str,
    /// Short identifier
    id: &'static str,
    /// Exact process name matches (case-insensitive)
    comm_exact: &'static [&'static str],
    /// Substrings to look for in the full cmdline (word-boundary match)
    cmdline_contains: &'static [&'static str],
    /// Icon emoji
    icon: &'static str,
    /// Category
    category: &'static str,
}

const AGENT_SIGNATURES: &[AgentSignature] = &[
    AgentSignature {
        name: "Claude Code",
        id: "claude-code",
        comm_exact: &["claude"],
        cmdline_contains: &["claude-code", "claude_code"],
        icon: "🟣",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "Codex",
        id: "codex",
        comm_exact: &["codex"],
        cmdline_contains: &["openai-codex", "codex-cli"],
        icon: "🟢",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "Gemini CLI",
        id: "gemini-cli",
        comm_exact: &["gemini"],
        cmdline_contains: &["gemini-cli", "@google/gemini-cli"],
        icon: "🔵",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "Cursor",
        id: "cursor",
        comm_exact: &["cursor", "Cursor", "cursor-agent"],
        cmdline_contains: &["cursor"],
        icon: "⚡",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "GitHub Copilot",
        id: "copilot",
        comm_exact: &["copilot"],
        cmdline_contains: &["github-copilot", "copilot-agent"],
        icon: "🤖",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "Aider",
        id: "aider",
        comm_exact: &["aider"],
        cmdline_contains: &["aider-chat", "aider"],
        icon: "🦊",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "Amp",
        id: "amp",
        comm_exact: &["amp"],
        cmdline_contains: &["ampcode", "amp-cli"],
        icon: "⚙️",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "Windsurf",
        id: "windsurf",
        comm_exact: &["windsurf", "Windsurf"],
        cmdline_contains: &["windsurf"],
        icon: "🏄",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "Zed",
        id: "zed",
        comm_exact: &["zed"],
        cmdline_contains: &["zed-editor"],
        icon: "✏️",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "OpenHands",
        id: "openhands",
        comm_exact: &["openhands"],
        cmdline_contains: &["openhands", "openhands-ai"],
        icon: "👐",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "Cline",
        id: "cline",
        comm_exact: &["cline"],
        cmdline_contains: &["cline-bot", "cline-extension"],
        icon: "📎",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "Qwen Code",
        id: "qwen-code",
        comm_exact: &["qwen", "qwen-code"],
        cmdline_contains: &["qwen-code", "qwen_code"],
        icon: "🔮",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "DeepSeek",
        id: "deepseek",
        comm_exact: &["deepseek"],
        cmdline_contains: &["deepseek-cli", "deepseek-code"],
        icon: "🐋",
        category: "Coding Agent",
    },
    AgentSignature {
        name: "Kilo Code",
        id: "kilo",
        comm_exact: &["kilo", "kilo-code"],
        cmdline_contains: &["kilocode", "kilo-code"],
        icon: "📏",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "Qoder",
        id: "qoder",
        comm_exact: &["qoder", "Qoder"],
        cmdline_contains: &["qoder"],
        icon: "🔧",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "QoderWork",
        id: "qoderwork",
        comm_exact: &["qoderwork", "QoderWork"],
        cmdline_contains: &["qoderwork", "qoder-work"],
        icon: "🏗️",
        category: "IDE Agent",
    },
    AgentSignature {
        name: "Roo Code",
        id: "roo",
        comm_exact: &["roo", "roo-code"],
        cmdline_contains: &["roo-code", "roo_code", "rooveter"],
        icon: "🦘",
        category: "IDE Agent",
    },
];

// ── Data structures ───────────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct AgentInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) icon: String,
    pub(crate) category: String,
    pub(crate) status: String,
    pub(crate) pids: Vec<u32>,
    pub(crate) process_count: usize,
    pub(crate) cpu_percent: f64,
    pub(crate) mem_mb: f64,
    pub(crate) uptime_secs: u64,
    pub(crate) cmdline_preview: String,
    pub(crate) cwd: String,
}

#[derive(Serialize)]
pub(crate) struct AgentsSummary {
    pub(crate) agents: Vec<AgentInfo>,
    pub(crate) total_running: usize,
    pub(crate) scanned_at: u64,
    pub(crate) hostname: String,
}

// ── Process scanning (cross-platform via sysinfo) ────────────────────────

struct ProcessInfo {
    pid: u32,
    comm: String,
    cmdline: String,
    cpu_percent: f64,
    mem_mb: f64,
    uptime_secs: u64,
    cwd: String,
}

/// Check if `haystack` contains `needle` as a whole word.
/// Word boundaries: non-alphanumeric, non-hyphen characters or string boundaries.
/// This prevents "qoder" from matching "qoder-server" or "qoderwork".
fn contains_word(haystack: &str, needle: &str) -> bool {
    let h = haystack.to_lowercase();
    let n = needle.to_lowercase();
    let mut start = 0;
    while let Some(pos) = h[start..].find(&n) {
        let abs_pos = start + pos;
        // Before: must be start-of-string, or preceded by non-alphanumeric non-hyphen
        let before_ok = abs_pos == 0
            || (!h.as_bytes()[abs_pos - 1].is_ascii_alphanumeric()
                && h.as_bytes()[abs_pos - 1] != b'-'
                && h.as_bytes()[abs_pos - 1] != b'_');
        // After: must be end-of-string, or followed by non-alphanumeric non-hyphen
        let after_pos = abs_pos + n.len();
        let after_ok = after_pos >= h.len()
            || (!h.as_bytes()[after_pos].is_ascii_alphanumeric()
                && h.as_bytes()[after_pos] != b'-'
                && h.as_bytes()[after_pos] != b'_');
        if before_ok && after_ok {
            return true;
        }
        start = abs_pos + 1;
    }
    false
}

/// Check if a PID is actually a thread (Tgid != Pid in /proc/[pid]/status).
/// On Linux, threads appear in /proc with their own TID but share memory with
/// the main process. We must skip them to avoid counting the same memory multiple times.
/// On macOS, sysinfo doesn't enumerate threads as separate processes, so this is a no-op.
fn is_thread(pid: u32) -> bool {
    if let Ok(status) = std::fs::read_to_string(crate::utils::procfs::proc_pid_entry(pid, "status"))
    {
        let mut tgid = None;
        let mut pid_val = None;
        for line in status.lines() {
            if line.starts_with("Tgid:") {
                tgid = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u32>().ok());
            } else if line.starts_with("Pid:") {
                pid_val = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u32>().ok());
            }
            if tgid.is_some() && pid_val.is_some() {
                break;
            }
        }
        if let (Some(tgid), Some(pid_val)) = (tgid, pid_val) {
            return tgid != pid_val;
        }
    }
    false
}

fn scan_processes() -> Vec<ProcessInfo> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    // sysinfo needs two CPU snapshots to compute usage %;
    // first call gets baseline, sleep briefly, then second call computes delta.
    // Use a short sleep to keep the endpoint responsive.
    std::thread::sleep(std::time::Duration::from_millis(50));
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut processes = Vec::new();

    for (pid, process) in sys.processes() {
        let pid_u32 = pid.as_u32();
        // Skip PID 1/2 (init/kernel)
        if pid_u32 <= 2 {
            continue;
        }

        let comm = process.name().to_string_lossy().to_string();

        // Skip threads: on Linux, threads have their own /proc/[tid]/ entries
        // with the same comm as the main process, but Tgid != Pid.
        if is_thread(pid_u32) {
            continue;
        }

        let cmdline: String = process
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ");

        let cwd = process
            .cwd()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let cpu_percent = process.cpu_usage() as f64;
        let mem_mb = process.memory() as f64 / (1024.0 * 1024.0);

        let start_time = process.start_time();
        let uptime_secs = if start_time > 0 && start_time <= now {
            now - start_time
        } else {
            0
        };

        processes.push(ProcessInfo {
            pid: pid_u32,
            comm,
            cmdline,
            cpu_percent,
            mem_mb,
            uptime_secs,
            cwd,
        });
    }

    processes
}

fn match_agents(processes: &[ProcessInfo]) -> Vec<AgentInfo> {
    let mut found: HashMap<&str, Vec<&ProcessInfo>> = HashMap::new();

    for proc in processes {
        // Skip our own agentsight process
        let comm_lower = proc.comm.to_lowercase();
        let cmdline_lower = proc.cmdline.to_lowercase();
        if comm_lower.contains("agentsight") || cmdline_lower.contains("agentsight") {
            continue;
        }

        for sig in AGENT_SIGNATURES {
            let mut matched = false;

            // Strategy 1: exact match on process name
            for exact in sig.comm_exact {
                if comm_lower == exact.to_lowercase() {
                    matched = true;
                    break;
                }
            }

            // Strategy 2: word-boundary match on cmdline
            if !matched {
                for pattern in sig.cmdline_contains {
                    if contains_word(&cmdline_lower, pattern) {
                        matched = true;
                        break;
                    }
                }
            }

            if matched {
                found.entry(sig.id).or_default().push(proc);
                break; // one agent per process
            }
        }
    }

    let mut agents: Vec<AgentInfo> = found
        .into_iter()
        .filter_map(|(id, procs)| {
            let sig = AGENT_SIGNATURES.iter().find(|s| s.id == id)?;
            let pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
            let cpu: f64 = procs.iter().map(|p| p.cpu_percent).sum();
            let mem_mb: f64 = procs.iter().map(|p| p.mem_mb).sum();
            let max_uptime = procs.iter().map(|p| p.uptime_secs).max().unwrap_or(0);
            let cmdline_preview = procs
                .first()
                .map(|p| {
                    let s = &p.cmdline;
                    if s.chars().count() > 120 {
                        format!("{}...", s.chars().take(120).collect::<String>())
                    } else {
                        s.clone()
                    }
                })
                .unwrap_or_default();
            let cwd = procs.first().map(|p| p.cwd.clone()).unwrap_or_default();

            Some(AgentInfo {
                id: sig.id.to_string(),
                name: sig.name.to_string(),
                icon: sig.icon.to_string(),
                category: sig.category.to_string(),
                status: "running".to_string(),
                pids,
                process_count: procs.len(),
                cpu_percent: (cpu * 10.0).round() / 10.0,
                mem_mb: (mem_mb * 10.0).round() / 10.0,
                uptime_secs: max_uptime,
                cmdline_preview,
                cwd,
            })
        })
        .collect();

    agents.sort_by_key(|agent| std::cmp::Reverse(agent.process_count));
    agents
}

fn get_hostname() -> String {
    System::host_name().unwrap_or_else(|| "unknown".to_string())
}

// ── Endpoint handler ──────────────────────────────────────────────────────

pub(crate) fn discover_agents_summary() -> AgentsSummary {
    let processes = scan_processes();
    let agents = match_agents(&processes);
    let total_running = agents.len();
    let scanned_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    AgentsSummary {
        agents,
        total_running,
        scanned_at,
        hostname: get_hostname(),
    }
}

/// List running AI agent processes detected on this machine.
#[get("/api/agents")]
pub async fn list_agents() -> impl Responder {
    let result = actix_web::web::block(discover_agents_summary).await;

    match result {
        Ok(summary) => HttpResponse::Ok().json(summary),
        Err(e) => HttpResponse::InternalServerError().body(format!("Process scan failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_proc(pid: u32, comm: &str, cmdline: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            comm: comm.to_string(),
            cmdline: cmdline.to_string(),
            cpu_percent: 1.0,
            mem_mb: 100.0,
            uptime_secs: 60,
            cwd: String::new(),
        }
    }

    // ── contains_word ──────────────────────────────────────────────────────

    #[test]
    fn contains_word_exact_match() {
        assert!(contains_word("claude", "claude"));
        assert!(contains_word("node /usr/bin/claude", "claude"));
    }

    #[test]
    fn contains_word_case_insensitive() {
        assert!(contains_word("CLAUDE CODE", "claude"));
        assert!(contains_word("Node Claude", "CLAUDE"));
    }

    #[test]
    fn contains_word_hyphen_is_word_char() {
        // Hyphen is treated as part of a word, so "qoder" should NOT match
        // "qoder-server" or "qoderwork"
        assert!(!contains_word("qoder-server", "qoder"));
        assert!(!contains_word("qoderwork", "qoder"));
        assert!(!contains_word("agentsight-serve", "agentsight"));
    }

    #[test]
    fn contains_word_underscore_is_word_char() {
        assert!(!contains_word("qoder_server", "qoder"));
        assert!(contains_word("qoder server", "qoder"));
    }

    #[test]
    fn contains_word_boundary_chars() {
        // Space, slash, and string boundaries are word boundaries
        assert!(contains_word("/usr/bin/qoder", "qoder"));
        assert!(contains_word("qoder --flag", "qoder"));
        assert!(!contains_word("qoderx", "qoder"));
    }

    #[test]
    fn contains_word_no_match() {
        assert!(!contains_word("vim", "claude"));
        assert!(!contains_word("", "claude"));
    }

    // ── match_agents ───────────────────────────────────────────────────────

    #[test]
    fn match_agents_by_comm_exact() {
        let procs = vec![make_proc(100, "claude", "node claude.js")];
        let agents = match_agents(&procs);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "claude-code");
        assert_eq!(agents[0].name, "Claude Code");
        assert_eq!(agents[0].pids, vec![100]);
        assert_eq!(agents[0].process_count, 1);
    }

    #[test]
    fn match_agents_by_cmdline_contains() {
        let procs = vec![make_proc(200, "node", "/usr/bin/node @google/gemini-cli")];
        let agents = match_agents(&procs);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "gemini-cli");
    }

    #[test]
    fn match_agents_skip_self() {
        let procs = vec![make_proc(1, "agentsight", "agentsight serve")];
        let agents = match_agents(&procs);
        assert!(agents.is_empty());
    }

    #[test]
    fn match_agents_skip_unknown() {
        let procs = vec![make_proc(300, "vim", "vim main.rs")];
        let agents = match_agents(&procs);
        assert!(agents.is_empty());
    }

    #[test]
    fn match_agents_aggregate_multiple_procs() {
        let procs = vec![
            make_proc(10, "claude", "node claude.js"),
            make_proc(11, "claude", "node claude.js"),
            make_proc(12, "node", "/usr/bin/node @google/gemini-cli"),
        ];
        let agents = match_agents(&procs);
        assert_eq!(agents.len(), 2);

        // Sorted by process_count descending — claude-code has 2, gemini-cli has 1
        assert_eq!(agents[0].id, "claude-code");
        assert_eq!(agents[0].process_count, 2);
        assert_eq!(agents[0].pids, vec![10, 11]);
        // CPU and memory should be summed
        assert_eq!(agents[0].cpu_percent, 2.0);
        assert_eq!(agents[0].mem_mb, 200.0);

        assert_eq!(agents[1].id, "gemini-cli");
        assert_eq!(agents[1].process_count, 1);
    }

    #[test]
    fn match_agents_no_false_positive_qoder() {
        // "qoder-server" should not match "qoder" signature
        let procs = vec![make_proc(400, "node", "node /opt/qoder-server/main.js")];
        let agents = match_agents(&procs);
        assert!(agents.is_empty());
    }

    #[test]
    fn match_agents_one_agent_per_process() {
        // A process that could match multiple signatures should only match the first
        let procs = vec![make_proc(500, "claude", "claude-code --agent aider")];
        let agents = match_agents(&procs);
        assert_eq!(agents.len(), 1);
        // claude-code (comm_exact) comes before aider in the signature list
        assert_eq!(agents[0].id, "claude-code");
    }

    #[test]
    fn discover_agents_summary_runs() {
        let summary = discover_agents_summary();
        assert!(summary.total_running <= summary.agents.len() || summary.agents.is_empty());
        assert!(summary.scanned_at > 0);
        assert!(!summary.hostname.is_empty());
    }
}
