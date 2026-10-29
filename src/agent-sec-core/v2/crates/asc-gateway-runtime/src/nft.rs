//! nftables ruleset that steers each Agent domain's HTTP(S) traffic to its listener.

use std::fmt::Write as _;

use asc_credential_store::AgentName;

/// Table owned exclusively by the gateway; every update replaces it whole.
pub(crate) const TABLE: &str = "agent_sec_gateway";

/// One running Agent domain whose scope cgroup exists.
pub(crate) struct Redirect<'a> {
    pub(crate) agent: &'a AgentName,
    /// cgroup v2 path relative to the hierarchy root, without a leading `/`.
    pub(crate) cgroup: &'a str,
    pub(crate) port: u16,
}

/// Renders the whole table as one `nft -f` transaction.
///
/// nft resolves each cgroup path to the cgroup's inode when loading, so the
/// table must be re-rendered whenever a scope is created again; a path whose
/// cgroup is missing fails the whole transaction and leaves the old table.
/// Only IPv4 TCP is supported; IPv6 and QUIC bypass credential protection.
pub(crate) fn render(redirects: &[Redirect<'_>]) -> String {
    let mut redirect_rules = String::new();
    for redirect in redirects {
        let level = redirect.cgroup.split('/').count();
        let matcher = format!("socket cgroupv2 level {level} \"{}\"", redirect.cgroup);
        let comment = format!("comment \"asc:agent:{}\"", redirect.agent);
        let _ = writeln!(
            redirect_rules,
            "    meta nfproto ipv4 {matcher} tcp dport {{ 80, 443 }} counter redirect to :{} {comment}",
            redirect.port
        );
    }
    format!(
        "table inet {TABLE}\n\
         delete table inet {TABLE}\n\
         table inet {TABLE} {{\n\
         \x20 chain redirect_out {{\n\
         \x20   type nat hook output priority -100; policy accept;\n\
         \x20   ip daddr 127.0.0.0/8 return\n\
         \x20   ip6 daddr ::1 return\n\
         {redirect_rules}\
         \x20 }}\n\
         }}\n"
    )
}

/// Transaction that removes the table if present.
pub(crate) fn render_delete() -> String {
    format!("table inet {TABLE}\ndelete table inet {TABLE}\n")
}

/// Whether `path` is safe to embed in a quoted nft string.
pub(crate) fn is_safe_cgroup_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@')
                })
        })
}
