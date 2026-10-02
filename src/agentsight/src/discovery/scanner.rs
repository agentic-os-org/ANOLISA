//! Agent process scanner
//!
//! This module provides functionality to scan the system for running AI agent processes
//! by examining procfs entries and handling process lifecycle events.
//! It also manages deny rules and domain rules for unified rule-based decisions.
//!
//! Every procfs read here resolves through [`crate::utils::procfs`], so an
//! observer in a container can be pointed at a bind-mounted host procfs.

use std::collections::HashMap;
use std::fs;

use super::agent::{AgentInfo, DiscoveredAgent};
use super::matcher::{CmdlineGlobMatcher, ProcessContext, match_domain_glob};
use crate::config::{CmdlineRule, HttpsRule};
use crate::utils::procfs::{proc_pid_entry, proc_root};

/// Scanner for discovering AI agent processes on the system
///
/// The scanner maintains allow matchers, deny matchers, and domain patterns.
/// It can scan the /proc filesystem to find running processes that match allow rules,
/// check deny rules, and match DNS domain events.
pub struct AgentScanner {
    /// Allow matchers (agent discovery)
    matchers: Vec<CmdlineGlobMatcher>,
    /// Deny matchers (blacklist)
    deny_matchers: Vec<CmdlineGlobMatcher>,
    /// Domain/DNS glob patterns
    domain_patterns: Vec<String>,
    /// Currently tracked agent processes: pid -> DiscoveredAgent
    tracked_agents: HashMap<u32, DiscoveredAgent>,
}

impl AgentScanner {
    /// Create a scanner from the full set of rules (recommended).
    ///
    /// Separates cmdline_rules into allow matchers and deny matchers,
    /// and stores domain patterns for DNS-based matching.
    pub fn from_rules(cmdline_rules: &[CmdlineRule], https_rules: &[HttpsRule]) -> Self {
        let matchers: Vec<CmdlineGlobMatcher> = cmdline_rules
            .iter()
            .filter_map(CmdlineGlobMatcher::from_config)
            .collect();
        let deny_matchers: Vec<CmdlineGlobMatcher> = cmdline_rules
            .iter()
            .filter_map(CmdlineGlobMatcher::from_deny_rule)
            .collect();
        let domain_patterns: Vec<String> = https_rules.iter().map(|r| r.pattern.clone()).collect();
        Self {
            matchers,
            deny_matchers,
            domain_patterns,
            tracked_agents: HashMap::new(),
        }
    }

    /// Check if cmdline matches any deny rule.
    pub fn is_denied(&self, cmdline_args: &[String]) -> bool {
        let ctx = ProcessContext {
            comm: String::new(),
            cmdline_args: cmdline_args.to_vec(),
            exe_path: String::new(),
        };
        self.deny_matchers.iter().any(|m| m.matches(&ctx))
    }

    /// Check if a domain matches any domain rule.
    pub fn matches_domain(&self, domain: &str) -> bool {
        match_domain_glob(domain, &self.domain_patterns)
    }

    /// Whether any domain rules are configured (used to enable UDP DNS probe).
    pub fn has_domain_rules(&self) -> bool {
        !self.domain_patterns.is_empty()
    }

    /// Get a reference to the domain patterns (used by ConnectionScanner)
    pub fn domain_patterns(&self) -> &[String] {
        &self.domain_patterns
    }

    /// Handle DNS query event: check domain match + deny check.
    ///
    /// Returns `true` if the process should be attached (domain matches and
    /// the process cmdline is not denied).
    pub fn on_dns_event(&self, pid: u32, domain: &str) -> bool {
        if !self.matches_domain(domain) {
            return false;
        }
        let cmdline = read_cmdline(pid);
        // Fail-closed: if cmdline is empty (process already exited or unreadable),
        // do NOT attach — deny rules cannot be evaluated reliably.
        if cmdline.is_empty() {
            log::debug!("on_dns_event: pid={pid} cmdline empty (process exited?), skipping attach");
            return false;
        }
        !self.is_denied(&cmdline)
    }

    /// Scan the system for running AI agent processes
    ///
    /// This method iterates over the `<procfs root>/[pid]/` directories and
    /// attempts to match each process against the known agent list based on
    /// process name.
    /// Discovered agents are automatically added to `tracked_agents`.
    ///
    /// # Returns
    ///
    /// A vector of `DiscoveredAgent` instances representing the found agent processes.
    pub fn scan(&mut self) -> Vec<DiscoveredAgent> {
        let mut discovered = Vec::new();

        // Read the procfs root directory
        let entries = match fs::read_dir(proc_root()) {
            Ok(e) => e,
            Err(_) => return discovered,
        };

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            // Only process numeric directory names (PIDs)
            let pid: u32 = match name_str.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Try to read process info and match against known agents
            if let Some(discovered_agent) = self.try_match_process(pid) {
                self.tracked_agents
                    .insert(discovered_agent.pid, discovered_agent.clone());
                discovered.push(discovered_agent);
            }
        }

        discovered
    }

    /// Handle process creation event
    ///
    /// Check if the new process matches a known agent and start tracking it.
    ///
    /// # Arguments
    /// * `pid` - Process ID
    /// * `bpf_comm` - Process command name (from BPF event, already updated at sys_exit_execve)
    ///
    /// # Returns
    ///
    /// `Some(DiscoveredAgent)` if the process is a known agent, `None` otherwise.
    pub fn on_process_create(&mut self, pid: u32, bpf_comm: &str) -> Option<&DiscoveredAgent> {
        // Use BPF comm as primary source (already updated at sys_exit_execve time).
        // Fallback to `<procfs root>/[pid]/comm` only if BPF comm is empty or too short.
        let comm = if bpf_comm.len() >= 3 {
            bpf_comm.to_string()
        } else {
            // Fallback: read from <procfs root>/[pid]/comm
            fs::read_to_string(proc_pid_entry(pid, "comm"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| bpf_comm.to_string())
        };

        // Read full command line from <procfs root>/[pid]/cmdline
        let cmdline_args = read_cmdline(pid);
        log::trace!("Process created: pid={pid}, comm='{comm}', cmdline={cmdline_args:?}");

        // Read executable path from <procfs root>/[pid]/exe (symlink)
        let exe = fs::read_link(proc_pid_entry(pid, "exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let ctx = ProcessContext {
            comm,
            cmdline_args: cmdline_args.clone(),
            exe_path: exe.clone(),
        };

        // Find the first matching agent
        let matched_info = self.find_match(&ctx)?;

        let discovered = DiscoveredAgent {
            agent_info: matched_info,
            pid,
            cmdline_args,
            exe_path: exe,
        };

        self.tracked_agents.insert(pid, discovered);
        self.tracked_agents.get(&pid)
    }

    /// Handle process exit event
    ///
    /// Remove the process from tracking if it was a known agent.
    pub fn on_process_exit(&mut self, pid: u32) -> Option<DiscoveredAgent> {
        log::trace!("Process exited: pid={pid}");
        self.tracked_agents.remove(&pid)
    }

    /// Check if a PID is currently being tracked
    pub fn is_tracked(&self, pid: u32) -> bool {
        self.tracked_agents.contains_key(&pid)
    }

    /// Get a tracked agent by PID
    pub fn get_tracked(&self, pid: u32) -> Option<&DiscoveredAgent> {
        self.tracked_agents.get(&pid)
    }

    /// Get all currently tracked agents
    pub fn tracked_agents(&self) -> &HashMap<u32, DiscoveredAgent> {
        &self.tracked_agents
    }

    /// Get list of tracked PIDs
    pub fn tracked_pids(&self) -> Vec<u32> {
        self.tracked_agents.keys().copied().collect()
    }

    /// Clear all tracked agents
    pub fn clear_tracked(&mut self) {
        self.tracked_agents.clear();
    }

    /// Attempt to match a process against known agents
    pub fn try_match_process(&self, pid: u32) -> Option<DiscoveredAgent> {
        // Read process name from <procfs root>/[pid]/comm
        let comm = fs::read_to_string(proc_pid_entry(pid, "comm")).ok()?;
        let process_name = comm.trim().to_string();

        // Read full command line from <procfs root>/[pid]/cmdline
        let cmdline_args = read_cmdline(pid);

        // Read executable path from <procfs root>/[pid]/exe (symlink)
        let exe = fs::read_link(proc_pid_entry(pid, "exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let ctx = ProcessContext {
            comm: process_name,
            cmdline_args: cmdline_args.clone(),
            exe_path: exe.clone(),
        };

        let matched_info = self.find_match(&ctx)?;

        Some(DiscoveredAgent {
            agent_info: matched_info,
            pid,
            cmdline_args,
            exe_path: exe,
        })
    }

    /// Find the first matching agent for a process context
    ///
    /// Deny rules take precedence over allow rules: without this check,
    /// agent-spawned helper subprocesses (e.g. an SFTP server launched
    /// through the agent's shell wrapper) match the broad allow rules and
    /// show up as discovered agents.
    fn find_match(&self, ctx: &ProcessContext) -> Option<AgentInfo> {
        if self.deny_matchers.iter().any(|m| m.matches(ctx)) {
            return None;
        }
        for matcher in &self.matchers {
            if matcher.matches(ctx) {
                return Some(matcher.info().clone());
            }
        }
        None
    }

    /// Get the number of registered agent matchers
    pub fn matcher_count(&self) -> usize {
        self.matchers.len()
    }
}

/// Read the process comm (`<procfs root>/<pid>/comm`), trimmed.
///
/// Returns `None` when the file is unreadable (process gone) or empty. This is
/// the *process* name (main-thread comm), not a per-event worker-thread name.
pub fn read_comm(pid: u32) -> Option<String> {
    fs::read_to_string(proc_pid_entry(pid, "comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read and parse a process's cmdline
///
/// The cmdline file contains arguments separated by null bytes.
/// Returns a vector of command line arguments, empty when the process is gone.
///
/// Takes the pid rather than a path so every caller resolves through the
/// configured procfs root (see [`crate::utils::procfs`]).
pub fn read_cmdline(pid: impl std::fmt::Display) -> Vec<String> {
    match fs::read(proc_pid_entry(pid, "cmdline")) {
        Ok(data) => {
            // Split by null bytes and collect non-empty strings
            data.split(|&b| b == 0)
                .filter_map(|slice| {
                    if slice.is_empty() {
                        None
                    } else {
                        Some(String::from_utf8_lossy(slice).into_owned())
                    }
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scanner_creation() {
        let scanner = AgentScanner::from_rules(&crate::config::default_cmdline_rules(), &[]);
        assert!(scanner.matcher_count() > 0);
    }

    #[test]
    fn test_process_lifecycle() {
        let mut scanner = AgentScanner::from_rules(&crate::config::default_cmdline_rules(), &[]);

        // Initially no tracked agents
        assert!(scanner.tracked_pids().is_empty());

        // Simulate process exit for non-tracked PID
        let result = scanner.on_process_exit(99999);
        assert!(result.is_none());

        // Check is_tracked
        assert!(!scanner.is_tracked(99999));
    }

    #[test]
    fn test_is_denied() {
        let rules = vec![CmdlineRule {
            patterns: vec!["*spam*".to_string()],
            agent_name: None,
            allow: false,
        }];
        let scanner = AgentScanner::from_rules(&rules, &[]);

        assert!(scanner.is_denied(&["spam-process".to_string()]));
        assert!(!scanner.is_denied(&["good-process".to_string()]));
    }

    #[test]
    fn test_matches_domain() {
        let https_rules = vec![
            HttpsRule {
                pattern: "*.openai.com".to_string(),
            },
            HttpsRule {
                pattern: "*.anthropic.com".to_string(),
            },
        ];
        let scanner = AgentScanner::from_rules(&[], &https_rules);

        assert!(scanner.matches_domain("api.openai.com"));
        assert!(scanner.matches_domain("api.anthropic.com"));
        assert!(!scanner.matches_domain("example.com"));
        assert!(scanner.has_domain_rules());
    }

    #[test]
    fn test_has_no_domain_rules() {
        let scanner = AgentScanner::from_rules(&crate::config::default_cmdline_rules(), &[]);
        assert!(!scanner.has_domain_rules());
    }

    #[test]
    fn test_from_rules_separates_allow_and_deny() {
        let rules = vec![
            CmdlineRule {
                patterns: vec!["node".to_string(), "*claude*".to_string()],
                agent_name: Some("Claude".to_string()),
                allow: true,
            },
            CmdlineRule {
                patterns: vec!["*deny-me*".to_string()],
                agent_name: None,
                allow: false,
            },
        ];
        let scanner = AgentScanner::from_rules(&rules, &[]);

        // One allow matcher
        assert_eq!(scanner.matcher_count(), 1);
        // Deny works
        assert!(scanner.is_denied(&["deny-me-process".to_string()]));
        assert!(!scanner.is_denied(&["node".to_string(), "/path/claude-code".to_string()]));
    }

    #[test]
    fn test_find_match_deny_takes_precedence_over_allow() {
        let rules = vec![
            CmdlineRule {
                patterns: vec!["node*".to_string(), "*agent*".to_string()],
                agent_name: Some("Agent".to_string()),
                allow: true,
            },
            CmdlineRule {
                patterns: vec!["*".to_string(), "*".to_string(), "-c".to_string()],
                agent_name: None,
                allow: false,
            },
        ];
        let scanner = AgentScanner::from_rules(&rules, &[]);

        let denied = ProcessContext {
            comm: String::new(),
            cmdline_args: vec![
                "node".to_string(),
                "/usr/lib/agent/cli.js".to_string(),
                "-c".to_string(),
                "/usr/libexec/openssh/sftp-server".to_string(),
            ],
            exe_path: String::new(),
        };
        assert!(scanner.find_match(&denied).is_none());

        let allowed = ProcessContext {
            comm: String::new(),
            cmdline_args: vec!["node".to_string(), "/usr/lib/agent/cli.js".to_string()],
            exe_path: String::new(),
        };
        assert_eq!(
            scanner.find_match(&allowed).map(|i| i.name),
            Some("Agent".to_string())
        );
    }

    #[test]
    fn test_default_rules_deny_sftp_subprocess() {
        let scanner = AgentScanner::from_rules(&crate::config::default_cmdline_rules(), &[]);

        // SFTP subprocess spawned through the cosh shell wrapper must not be
        // reported as a Cosh agent.
        let sftp = ProcessContext {
            comm: String::new(),
            cmdline_args: vec![
                "node".to_string(),
                "/usr/lib/copilot-shell/cli.js".to_string(),
                "-c".to_string(),
                "/usr/libexec/openssh/sftp-server".to_string(),
            ],
            exe_path: String::new(),
        };
        assert!(scanner.find_match(&sftp).is_none());

        // A genuine interactive cosh process still matches.
        let cosh = ProcessContext {
            comm: String::new(),
            cmdline_args: vec![
                "node".to_string(),
                "/usr/lib/copilot-shell/cli.js".to_string(),
            ],
            exe_path: String::new(),
        };
        assert_eq!(
            scanner.find_match(&cosh).map(|i| i.name),
            Some("Cosh".to_string())
        );
    }

    #[test]
    fn test_read_comm_current_process() {
        // The current process comm is readable and non-empty.
        let comm = read_comm(std::process::id());
        assert!(comm.is_some());
        assert!(!comm.unwrap().is_empty());
    }

    #[test]
    fn test_read_comm_bogus_pid() {
        // A pid that cannot exist yields None (no panic, no empty string).
        assert!(read_comm(u32::MAX).is_none());
    }

    #[test]
    fn test_try_match_process_current() {
        let scanner = AgentScanner::from_rules(&crate::config::default_cmdline_rules(), &[]);
        // The current test process should not match any agent rule.
        let result = scanner.try_match_process(std::process::id());
        assert!(result.is_none());
    }

    #[test]
    fn test_scan_without_rules_finds_nothing() {
        let mut scanner = AgentScanner::from_rules(&[], &[]);
        // Exercises the full procfs walk; with no rules nothing can match.
        assert!(scanner.scan().is_empty());
    }

    #[test]
    fn test_on_dns_event_unreadable_pid_fails_closed() {
        let https_rules = vec![crate::config::HttpsRule {
            pattern: "example.com".to_string(),
        }];
        let scanner = AgentScanner::from_rules(&[], &https_rules);
        // The domain matches, but the (impossible) pid's cmdline is unreadable:
        // deny rules cannot be evaluated, so no attach happens.
        assert!(scanner.matches_domain("example.com"));
        assert!(!scanner.on_dns_event(u32::MAX, "example.com"));
    }

    #[test]
    fn test_on_process_create_unreadable_pid_matches_nothing() {
        let mut scanner = AgentScanner::from_rules(&[], &[]);
        // u32::MAX exceeds the kernel's pid_max, so every procfs read fails;
        // the short BPF comm forces the comm-read fallback as well.
        assert!(scanner.on_process_create(u32::MAX, "x").is_none());
    }
}
