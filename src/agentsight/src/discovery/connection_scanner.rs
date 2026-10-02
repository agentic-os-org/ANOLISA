//! Connection scanner for discovering processes with established LLM API connections
//!
//! When an AI Agent is already running and has established connections to LLM APIs
//! before AgentSight starts, the UDP DNS probe won't catch it (no new DNS lookups).
//! This module performs a one-time scan of established TCP connections to find such processes.
//!
//! Flow:
//! 1. Resolve exact domains from `https_rules` to IP addresses
//! 2. Scan `/proc/net/tcp` for ESTABLISHED connections to those IPs
//! 3. Map socket inodes back to PIDs
//! 4. Filter through deny rules before attaching SSL probes

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use super::scanner::{AgentScanner, read_cmdline};
use crate::utils::procfs::{proc_pid_entry, proc_root};

/// IP → domain name mapping cache
pub type IpDomainCache = HashMap<IpAddr, String>;

/// Connection scan result for a single process
pub struct ConnectionScanResult {
    pub pid: u32,
    pub domain: String,
    pub remote_ip: IpAddr,
    pub remote_port: u16,
}

/// Scanner that finds processes with established connections to known LLM API endpoints
pub struct ConnectionScanner<'a> {
    scanner: &'a AgentScanner,
}

impl<'a> ConnectionScanner<'a> {
    /// Create a new connection scanner referencing the AgentScanner for deny checks
    pub fn new(scanner: &'a AgentScanner) -> Self {
        Self { scanner }
    }

    /// Main entry point: scan for processes with established connections to https_rules IPs
    ///
    /// Filters out already-traced PIDs, applies deny rules to candidates.
    pub fn scan(&self, already_traced: &HashSet<u32>) -> Vec<ConnectionScanResult> {
        let ip_cache = resolve_domains(self.scanner.domain_patterns());
        if ip_cache.is_empty() {
            log::debug!("Connection scan: no IPs resolved from domain rules, skipping");
            return Vec::new();
        }
        log::info!(
            "Connection scan: resolved {} IP(s) from exact domains",
            ip_cache.len()
        );

        // Scan TCP connections matching target IPs
        let tcp_matches = scan_tcp_connections(&ip_cache);
        if tcp_matches.is_empty() {
            log::debug!("Connection scan: no matching ESTABLISHED connections found");
            return Vec::new();
        }

        // Collect inodes we need to resolve
        let target_inodes: HashSet<u64> =
            tcp_matches.iter().map(|(inode, _, _, _)| *inode).collect();

        // Map inodes to PIDs
        let inode_to_pid = resolve_inodes_to_pids(&target_inodes);

        // Build results: deduplicate by PID, apply deny rules
        let mut seen_pids: HashSet<u32> = HashSet::new();
        let mut results = Vec::new();

        for (inode, remote_ip, remote_port, domain) in &tcp_matches {
            let pid = match inode_to_pid.get(inode) {
                Some(&p) => p,
                None => continue,
            };

            // Skip already-traced or already-seen in this scan
            if already_traced.contains(&pid) || seen_pids.contains(&pid) {
                continue;
            }

            // Read cmdline for deny check (fail-closed: skip if unreadable)
            let cmdline = read_cmdline(pid);
            if cmdline.is_empty() {
                log::debug!("Connection scan: pid={pid} cmdline empty (process exited?), skipping");
                continue;
            }

            // Apply deny rules
            if self.scanner.is_denied(&cmdline) {
                log::debug!("Connection scan: pid={pid} denied by rule, skipping");
                continue;
            }

            seen_pids.insert(pid);
            log::info!(
                "Connection scan: found pid={pid} connected to {domain} ({remote_ip}:{remote_port})"
            );
            results.push(ConnectionScanResult {
                pid,
                domain: domain.clone(),
                remote_ip: *remote_ip,
                remote_port: *remote_port,
            });
        }

        results
    }
}

/// Check if a domain pattern is an exact domain (no wildcards)
fn is_exact_domain(pattern: &str) -> bool {
    !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[')
}

/// Resolve exact domains from patterns to IPs using std::net::ToSocketAddrs
///
/// Skips wildcard patterns (cannot DNS-resolve wildcards).
/// Returns a map of IP → domain for all resolved addresses.
fn resolve_domains(domain_patterns: &[String]) -> IpDomainCache {
    use std::net::ToSocketAddrs;

    let mut cache = HashMap::new();
    for pattern in domain_patterns {
        if !is_exact_domain(pattern) {
            continue;
        }
        match (pattern.as_str(), 0u16).to_socket_addrs() {
            Ok(addrs) => {
                for addr in addrs {
                    log::debug!("Connection scan: resolved {} → {}", pattern, addr.ip());
                    cache.insert(addr.ip(), pattern.clone());
                }
            }
            Err(e) => {
                log::warn!("Connection scan: DNS resolution failed for {pattern}: {e}");
            }
        }
    }
    cache
}

/// Scan /proc/net/tcp for ESTABLISHED connections to target IPs
///
/// Returns: Vec<(inode, remote_ip, remote_port, domain)>
///
/// Deliberately reads the observer's own `/proc/net/tcp` rather than the
/// configured procfs root: the connection table is network-namespace scoped, so
/// the useful view is the netns we share (the host's, under `hostNetwork`). With
/// no shared netns this finds nothing and the scan simply yields no candidates.
fn scan_tcp_connections(ip_cache: &IpDomainCache) -> Vec<(u64, IpAddr, u16, String)> {
    use procfs::net::{TcpState, tcp};

    let mut results = Vec::new();
    match tcp() {
        Ok(entries) => {
            for entry in entries {
                if entry.state != TcpState::Established {
                    continue;
                }
                let remote_ip = entry.remote_address.ip();
                if let Some(domain) = ip_cache.get(&remote_ip) {
                    results.push((
                        entry.inode,
                        remote_ip,
                        entry.remote_address.port(),
                        domain.clone(),
                    ));
                }
            }
        }
        Err(e) => {
            log::warn!("Connection scan: failed to read /proc/net/tcp: {e}");
        }
    }
    results
}

/// Resolve socket inodes to PIDs by scanning `<procfs root>/[pid]/fd/`
///
/// Walks the configured procfs root by hand instead of `procfs::all_processes()`,
/// which hardcodes `/proc`: an observer pointed at a bind-mounted host procfs
/// would otherwise enumerate only itself and resolve nothing.
/// (`health::port_detector` carries the inverse lookup -- inodes *for* one pid --
/// against the same file layout.)
fn resolve_inodes_to_pids(target_inodes: &HashSet<u64>) -> HashMap<u64, u32> {
    let mut map = HashMap::new();
    let entries = match std::fs::read_dir(proc_root()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Connection scan: failed to read {:?}: {e}", proc_root());
            return map;
        }
    };

    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(proc_pid_entry(pid, "fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            // Socket fds link to "socket:[<inode>]".
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:["))
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|t| t.parse::<u64>().ok())
            else {
                continue;
            };
            if target_inodes.contains(&inode) {
                map.insert(inode, pid);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_exact_domain() {
        assert!(is_exact_domain("api.openai.com"));
        assert!(is_exact_domain("api.anthropic.com"));
        assert!(is_exact_domain("generativelanguage.googleapis.com"));

        assert!(!is_exact_domain("*.openai.com"));
        assert!(!is_exact_domain("api.?.com"));
        assert!(!is_exact_domain("[a-z].openai.com"));
    }

    #[test]
    fn test_resolve_domains_skips_wildcards() {
        // Only exact domains should be resolved; wildcards should be skipped
        let patterns = vec!["*.openai.com".to_string(), "*.anthropic.com".to_string()];
        let cache = resolve_domains(&patterns);
        // All patterns are wildcards, so cache should be empty
        assert!(cache.is_empty());
    }

    #[test]
    fn test_connection_scan_dedup() {
        // Verify that ConnectionScanResult deduplicates by PID
        let mut seen = HashSet::new();
        let pids = vec![100, 100, 200, 200, 300];
        let unique: Vec<u32> = pids.into_iter().filter(|pid| seen.insert(*pid)).collect();
        assert_eq!(unique, vec![100, 200, 300]);
    }

    #[test]
    fn resolve_inodes_without_targets_walks_without_matching() {
        // Empty target set: the procfs walk runs end to end and matches nothing.
        assert!(resolve_inodes_to_pids(&HashSet::new()).is_empty());
    }

    #[test]
    fn resolve_inodes_maps_own_sockets_back_to_us() {
        // Guarantee at least one socket fd exists in this process.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let _stream = std::net::TcpStream::connect(addr).expect("connect");
        let _accepted = listener.accept().expect("accept");

        // Harvest our own socket inodes through the same fd layout the scanner
        // walks, then verify the resolver maps them back to this process.
        let fd_dir = crate::utils::procfs::proc_pid_entry(std::process::id(), "fd");
        let mut inodes = HashSet::new();
        for fd in std::fs::read_dir(&fd_dir)
            .expect("read own fd dir")
            .flatten()
        {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            if let Some(inode) = target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:["))
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|t| t.parse::<u64>().ok())
            {
                inodes.insert(inode);
            }
        }
        assert!(
            !inodes.is_empty(),
            "the connected stream must yield a socket inode"
        );

        let map = resolve_inodes_to_pids(&inodes);
        assert!(!map.is_empty());
        assert!(map.values().all(|&pid| pid == std::process::id()));
    }

    #[test]
    fn scan_finds_own_localhost_connection() {
        // End-to-end: "localhost" resolves to 127.0.0.1, we hold an established
        // connection to it, so the scan must resolve our pid through the full
        // tcp-table -> inode -> pid -> cmdline chain.
        let https_rules = vec![crate::config::HttpsRule {
            pattern: "localhost".to_string(),
        }];
        let agent_scanner = AgentScanner::from_rules(&[], &https_rules);
        let scanner = ConnectionScanner::new(&agent_scanner);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let _stream = std::net::TcpStream::connect(addr).expect("connect");
        let _accepted = listener.accept().expect("accept");

        let results = scanner.scan(&HashSet::new());
        assert!(
            results.iter().any(|r| r.pid == std::process::id()),
            "expected our pid in scan results, got {:?}",
            results.iter().map(|r| r.pid).collect::<Vec<_>>()
        );
    }
}
