//! Dashboard capability detection.
//!
//! The frontend NavBar hides entries whose capability is absent from the
//! `/api/auth/status` response. Capabilities backed by companion components
//! (agent-sec-core, the enforcer service, tokenless) are probed here so a
//! deployment that ships only agentsight does not advertise features it
//! cannot serve.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Well-known install directories probed in addition to `PATH`, so a process
/// launched with a stripped environment (systemd unit, container entrypoint)
/// still detects installed components.
const FALLBACK_BIN_DIRS: &[&str] = &["/usr/local/bin", "/usr/bin", "/usr/sbin"];

/// Binary name installed by the agentsight packaging for the enforcer daemon.
const ENFORCER_BINARY: &str = "agentsight-enforcer";

/// Resolve the enforcer service socket path (env override or default).
///
/// Shared with `run_server` so the capability probe and the enforcement
/// client always agree on the location.
pub(crate) fn enforcer_socket_path() -> PathBuf {
    std::env::var_os("AGENTSIGHT_ENFORCER_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/agentsight/enforcer.sock"))
}

/// Probe companion components and return the capability list to advertise.
///
/// Re-evaluated per request: probes are cheap filesystem checks, and a
/// daemon started after agentsight simply appears on the next page load.
pub(crate) fn app_capabilities() -> Vec<&'static str> {
    detect_capabilities(
        agent_sec_available(),
        enforcement_available(),
        token_savings_available(),
    )
}

/// Assemble the capability list from probe results.
///
/// Capabilities served by agentsight itself are unconditional; the
/// component-backed ones are gated by their probes. `system_audit` is
/// derived: the audit store and its enforcer-fed ingestion live inside
/// agentsight, so the page is useful whenever either event source exists.
/// Order mirrors the dashboard navigation.
fn detect_capabilities(
    security: bool,
    enforcement: bool,
    token_savings: bool,
) -> Vec<&'static str> {
    let mut caps = vec!["agent_observability", "sessions"];
    if token_savings {
        caps.push("token_savings");
    }
    caps.extend(["optimization", "skills"]);
    if security {
        caps.push("security");
    }
    if security || enforcement {
        caps.push("system_audit");
    }
    if enforcement {
        caps.push("enforcement");
    }
    // Unconditional like `atif`: `reuse.db` degrades gracefully, and the page
    // reports why labels are unavailable rather than vanishing from the menu.
    caps.extend(["reuse_labels", "atif", "settings", "agent_health"]);
    caps
}

/// agent-sec-core backs the security observability and system audit pages.
///
/// A live daemon socket is the strongest signal; fall back to the binaries
/// being installed so an installed-but-stopped daemon still shows the pages
/// (which render their own "daemon unavailable" state).
fn agent_sec_available() -> bool {
    let socket_exists = crate::agent_sec::AgentSecClient::new(None)
        .map(|client| client.socket_path().exists())
        .unwrap_or(false);
    socket_exists || binary_installed("agent-sec-daemon") || binary_installed("agent-sec-cli")
}

/// The enforcer backs risk enforcement: a live socket is the strongest
/// signal, but keep the capability while the daemon is restarting so the
/// page's own readiness diagnostics stay reachable — gate on the installed
/// binary as well.
fn enforcement_available() -> bool {
    enforcer_socket_path().exists() || binary_installed(ENFORCER_BINARY)
}

/// tokenless backs the token savings page: stats.db appears once it has
/// optimized something; the binary alone means it can start producing data.
fn token_savings_available() -> bool {
    crate::storage::sqlite::tokenless::default_stats_path().exists()
        || binary_installed("tokenless")
}

/// Whether an executable named `name` is reachable via `PATH` or one of the
/// well-known install directories.
fn binary_installed(name: &str) -> bool {
    binary_in_path(name, std::env::var_os("PATH").as_deref())
        || FALLBACK_BIN_DIRS
            .iter()
            .any(|dir| is_executable(&Path::new(dir).join(name)))
}

/// Whether an executable named `name` exists in any `path_var` directory.
fn binary_in_path(name: &str, path_var: Option<&OsStr>) -> bool {
    let Some(paths) = path_var else {
        return false;
    };
    std::env::split_paths(paths).any(|dir| {
        if dir.as_os_str().is_empty() {
            return false;
        }
        is_executable(&dir.join(name))
    })
}

/// Whether `path` is an existing regular file with any execute bit set.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_all_probes_false_keeps_only_intrinsic_capabilities() {
        let caps = detect_capabilities(false, false, false);
        assert_eq!(
            caps,
            vec![
                "agent_observability",
                "sessions",
                "optimization",
                "skills",
                "reuse_labels",
                "atif",
                "settings",
                "agent_health",
            ]
        );
    }

    #[test]
    fn detect_all_probes_true_yields_full_list_in_nav_order() {
        let caps = detect_capabilities(true, true, true);
        assert_eq!(
            caps,
            vec![
                "agent_observability",
                "sessions",
                "token_savings",
                "optimization",
                "skills",
                "security",
                "system_audit",
                "enforcement",
                "reuse_labels",
                "atif",
                "settings",
                "agent_health",
            ]
        );
    }

    #[test]
    fn detect_gates_each_capability_independently() {
        assert!(detect_capabilities(true, false, false).contains(&"security"));
        assert!(!detect_capabilities(true, false, false).contains(&"enforcement"));
        assert!(detect_capabilities(false, true, false).contains(&"enforcement"));
        assert!(detect_capabilities(false, false, true).contains(&"token_savings"));
    }

    #[test]
    fn system_audit_follows_either_event_source() {
        // Audit ingestion is fed by the enforcer, and agent-sec deployments
        // used the audit page before this gating existed — either source
        // must keep it reachable.
        assert!(detect_capabilities(true, false, false).contains(&"system_audit"));
        assert!(detect_capabilities(false, true, false).contains(&"system_audit"));
        assert!(!detect_capabilities(false, false, true).contains(&"system_audit"));
    }

    #[test]
    fn binary_in_path_finds_executable_and_rejects_others() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("agentsight-caps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fake-tool");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let non_exec = dir.join("plain-file");
        std::fs::write(&non_exec, "data").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();

        let path_var = std::env::join_paths([&dir]).unwrap();
        assert!(binary_in_path("fake-tool", Some(path_var.as_os_str())));
        assert!(!binary_in_path("plain-file", Some(path_var.as_os_str())));
        assert!(!binary_in_path("absent-tool", Some(path_var.as_os_str())));
        assert!(!binary_in_path("fake-tool", None));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
