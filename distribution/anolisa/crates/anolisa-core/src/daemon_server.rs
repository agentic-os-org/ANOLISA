//! Daemon server for the ANOLISA system-helper.
//!
//! Listens on a Unix socket, authenticates peers via `SO_PEERCRED`,
//! dispatches requests through the operation white-list and rate limiter,
//! then delegates to domain-specific handlers (osbase install, list, etc.).
//!
//! Designed to run as a systemd `Type=simple` service in the foreground.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use anolisa_platform::ipc::{PeerCredential, get_peer_credential, recv_message, send_message};

use crate::system_helper::{
    HelperRequest, HelperResponse, OperationType, RateLimiter, is_operation_allowed, operation_type,
};

// ─── Constants ───────────────────────────────────────────────────────────────

const DEFAULT_RATE_LIMIT: usize = 30;
const AUDIT_LOG_DIR: &str = "/var/log/anolisa";
const AUDIT_LOG_PATH: &str = "/var/log/anolisa/system-helper.log";

// ─── DaemonServer ────────────────────────────────────────────────────────────

/// The system-helper daemon server.
///
/// Accepts connections on a Unix socket, validates peer credentials, enforces
/// rate limits and operation white-lists, and dispatches to domain handlers.
pub struct DaemonServer {
    socket_path: String,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    version: String,
    start_time: Instant,
    last_operation: Arc<Mutex<Option<(String, String)>>>,
    shutdown: Arc<AtomicBool>,
}

impl DaemonServer {
    /// Change group ownership of a path to the `anolisa` system group.
    /// Silently succeeds if the group doesn't exist (e.g. in tests).
    fn chgrp_anolisa(path: &std::path::Path) -> io::Result<()> {
        use std::process::Command;
        let status = Command::new("chgrp").arg("anolisa").arg(path).status();
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(_) => {
                eprintln!(
                    "[anolisa-helper] warning: chgrp anolisa {path:?} failed (group may not exist)"
                );
                Ok(()) // non-fatal
            }
            Err(e) => {
                eprintln!("[anolisa-helper] warning: chgrp command failed: {e}");
                Ok(()) // non-fatal
            }
        }
    }

    /// Create a new daemon server bound to the given socket path.
    pub fn new(socket_path: &str) -> Self {
        Self {
            socket_path: socket_path.to_string(),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new(DEFAULT_RATE_LIMIT))),
            version: env!("CARGO_PKG_VERSION").to_string(),
            start_time: Instant::now(),
            last_operation: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the main accept loop.
    ///
    /// 1. Create `/run/anolisa/` directory if absent.
    /// 2. Remove stale socket file if present.
    /// 3. Bind the `UnixListener`.
    /// 4. Set socket file permissions to `0o660`.
    /// 5. Loop accepting connections, spawning a thread per connection.
    pub fn run(&self) -> io::Result<()> {
        // Ensure socket directory exists.
        let socket_dir = std::path::Path::new(&self.socket_path)
            .parent()
            .unwrap_or(std::path::Path::new("/run/anolisa"));
        fs::create_dir_all(socket_dir)?;

        // Set directory and socket group to `anolisa` so group members can connect.
        Self::chgrp_anolisa(socket_dir)?;
        fs::set_permissions(socket_dir, fs::Permissions::from_mode(0o750))?;

        // Remove stale socket.
        if std::path::Path::new(&self.socket_path).exists() {
            fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        // Set socket permissions: owner + group read/write (0660), group = anolisa.
        fs::set_permissions(&self.socket_path, fs::Permissions::from_mode(0o660))?;
        Self::chgrp_anolisa(std::path::Path::new(&self.socket_path))?;

        // Set a non-blocking accept timeout so we can check the shutdown flag.
        listener.set_nonblocking(false)?;

        eprintln!(
            "[anolisa-helper] listening on {} (v{})",
            self.socket_path, self.version
        );

        for stream in listener.incoming() {
            if self.shutdown.load(Ordering::Relaxed) {
                break;
            }

            match stream {
                Ok(stream) => {
                    let rate_limiter = Arc::clone(&self.rate_limiter);
                    let last_operation = Arc::clone(&self.last_operation);
                    let shutdown = Arc::clone(&self.shutdown);
                    let version = self.version.clone();
                    let start_time = self.start_time;

                    thread::spawn(move || {
                        if let Err(e) = handle_connection(
                            stream,
                            &rate_limiter,
                            &last_operation,
                            &shutdown,
                            &version,
                            start_time,
                        ) {
                            eprintln!("[anolisa-helper] connection error: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[anolisa-helper] accept error: {e}");
                    continue;
                }
            }
        }

        // Cleanup socket on exit.
        let _ = fs::remove_file(&self.socket_path);
        eprintln!("[anolisa-helper] shutdown complete");
        Ok(())
    }

    /// Signal the server to stop accepting new connections.
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

// ─── Connection handler ──────────────────────────────────────────────────────

/// Handle a single client connection (runs in its own thread).
fn handle_connection(
    mut stream: UnixStream,
    rate_limiter: &Arc<Mutex<RateLimiter>>,
    last_operation: &Arc<Mutex<Option<(String, String)>>>,
    shutdown: &Arc<AtomicBool>,
    version: &str,
    start_time: Instant,
) -> io::Result<()> {
    let peer = get_peer_credential(&stream)?;

    // First message must be a Handshake.
    let first: HelperRequest = recv_message(&mut stream)?;
    match &first {
        HelperRequest::Handshake { cli_version } => {
            let compatible = is_compatible(cli_version, version);
            let resp = HelperResponse::HandshakeOk {
                helper_version: version.to_string(),
                compatible,
            };
            send_message(&mut stream, &resp)?;
            if !compatible {
                return Ok(());
            }
        }
        _ => {
            let resp = HelperResponse::Error {
                code: "PROTOCOL_ERROR".to_string(),
                message: "first message must be Handshake".to_string(),
            };
            send_message(&mut stream, &resp)?;
            return Ok(());
        }
    }

    // Message loop.
    loop {
        let req: HelperRequest = match recv_message(&mut stream) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };

        let op_start = Instant::now();
        let resp = dispatch(
            &req,
            &peer,
            rate_limiter,
            last_operation,
            shutdown,
            version,
            start_time,
        );
        let duration_ms = op_start.elapsed().as_millis() as u64;

        // Audit log.
        let exit_code = match &resp {
            HelperResponse::Success { exit_code, .. } => *exit_code,
            HelperResponse::Error { .. } => 1,
            _ => 0,
        };
        let op_name = format!("{:?}", operation_type(&req));
        let op_args = request_args(&req);
        write_audit_log(&peer, &op_name, &op_args, exit_code, duration_ms);

        send_message(&mut stream, &resp)?;

        // Handle shutdown request.
        if matches!(req, HelperRequest::Shutdown) {
            break;
        }
    }

    Ok(())
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Route a validated request to the appropriate handler.
fn dispatch(
    req: &HelperRequest,
    peer: &PeerCredential,
    rate_limiter: &Arc<Mutex<RateLimiter>>,
    last_operation: &Arc<Mutex<Option<(String, String)>>>,
    shutdown: &Arc<AtomicBool>,
    version: &str,
    start_time: Instant,
) -> HelperResponse {
    let op = operation_type(req);

    // Rate limit check (skip for Handshake — already handled).
    if op != OperationType::Handshake
        && let Ok(mut rl) = rate_limiter.lock()
        && let Err(msg) = rl.check(peer.uid)
    {
        return HelperResponse::Error {
            code: "RATE_LIMITED".to_string(),
            message: msg,
        };
    }

    // White-list check.
    if !is_operation_allowed(op, peer.uid) {
        return HelperResponse::Error {
            code: "PERMISSION_DENIED".to_string(),
            message: format!("operation {:?} not allowed for uid {}", op, peer.uid),
        };
    }

    // Track last operation.
    {
        if let Ok(mut last) = last_operation.lock() {
            let ts = chrono::Utc::now().to_rfc3339();
            *last = Some((format!("{op:?}"), ts));
        }
    }

    match req {
        HelperRequest::Handshake { .. } => {
            // Should not reach here in normal flow.
            HelperResponse::Error {
                code: "PROTOCOL_ERROR".to_string(),
                message: "unexpected duplicate handshake".to_string(),
            }
        }

        HelperRequest::OsbaseInstall {
            scenario,
            register_handler,
            register_runtimeclass,
            config_override,
            set_default,
            force,
            skip_verify,
            dry_run,
            ..
        } => dispatch_osbase_install(
            scenario,
            register_handler,
            *register_runtimeclass,
            config_override.as_deref(),
            *set_default,
            *force,
            *skip_verify,
            *dry_run,
        ),

        HelperRequest::OsbaseList { .. } => dispatch_osbase_list(),

        HelperRequest::OsbaseStatus { .. } => HelperResponse::Error {
            code: "NOT_IMPLEMENTED".to_string(),
            message: "osbase status via helper not yet implemented".to_string(),
        },

        HelperRequest::OsbaseDoctor { .. } => HelperResponse::Error {
            code: "NOT_IMPLEMENTED".to_string(),
            message: "osbase doctor via helper not yet implemented".to_string(),
        },

        HelperRequest::OsbaseRemove { .. } => HelperResponse::Error {
            code: "NOT_IMPLEMENTED".to_string(),
            message: "osbase remove via helper not yet implemented".to_string(),
        },

        HelperRequest::OsbaseUninstall { scenario, dry_run } => {
            dispatch_osbase_uninstall(scenario, *dry_run)
        }

        HelperRequest::OsbaseSetDefault { .. } => HelperResponse::Error {
            code: "NOT_IMPLEMENTED".to_string(),
            message: "osbase set-default via helper not yet implemented".to_string(),
        },

        HelperRequest::WsCkptSnapshot { .. } | HelperRequest::WsCkptRestore { .. } => {
            HelperResponse::Error {
                code: "NOT_IMPLEMENTED".to_string(),
                message: "ws-ckpt operations not yet implemented".to_string(),
            }
        }

        HelperRequest::SystemStatus => {
            let uptime_secs = start_time.elapsed().as_secs();
            let (last_op, last_op_time) = last_operation
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .map(|(op, ts)| (Some(op), Some(ts)))
                .unwrap_or((None, None));
            HelperResponse::Status {
                running: true,
                version: version.to_string(),
                uptime_secs,
                last_operation: last_op,
                last_operation_time: last_op_time,
            }
        }

        HelperRequest::Shutdown => {
            shutdown.store(true, Ordering::Relaxed);
            HelperResponse::Success {
                message: "shutdown initiated".to_string(),
                exit_code: 0,
            }
        }
    }
}

// ─── OsbaseInstall dispatch ──────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn dispatch_osbase_install(
    scenario: &str,
    register_handler: &str,
    register_runtimeclass: bool,
    config_override: Option<&str>,
    set_default: bool,
    force: bool,
    skip_verify: bool,
    dry_run: bool,
) -> HelperResponse {
    use crate::osbase_install::{
        OsbaseDomain, OsbaseInstallRequest, RegisterHandler, execute_install,
    };

    let handler = match register_handler {
        "containerd" => RegisterHandler::Containerd,
        "none" | "" => RegisterHandler::None,
        other => {
            return HelperResponse::Error {
                code: "INVALID_ARGUMENT".to_string(),
                message: format!("unknown register_handler: {other}"),
            };
        }
    };

    let request = OsbaseInstallRequest {
        domain: OsbaseDomain::Sandbox,
        target: scenario.to_string(),
        register_handler: handler,
        register_runtimeclass,
        config_override: config_override.map(|s| s.to_string()),
        set_default,
        force,
        skip_verify,
        dry_run,
    };

    let env = anolisa_env::EnvService::detect();

    match execute_install(&request, &env) {
        Ok(outcome) => format_outcome_response(outcome),
        Err(e) => HelperResponse::Error {
            code: "EXECUTION_FAILED".to_string(),
            message: format!("{e}"),
        },
    }
}

/// Format an `OsbaseInstallOutcome` into a `HelperResponse::Success`.
///
/// Renders every phase from `outcome.phases` so the non-root CLI path
/// sees the full five-phase pipeline result (preflight, packages,
/// services, verify, state) rather than a partial reconstruction.
fn format_outcome_response(outcome: crate::osbase_install::OsbaseInstallOutcome) -> HelperResponse {
    use crate::osbase_install::PhaseStatus;
    let mut lines = Vec::new();

    for phase in &outcome.phases {
        let status_str = match phase.status {
            PhaseStatus::Success => "\u{2713}",
            PhaseStatus::Skipped => "skipped",
            PhaseStatus::Degraded => "degraded",
            PhaseStatus::Failed => "\u{2717}",
        };
        let msg = phase.message.as_deref().unwrap_or("");
        lines.push(format!("{}: {} {}", phase.name, status_str, msg));
    }

    // Append real warnings if any.
    for w in &outcome.warnings {
        lines.push(format!("warning: {w}"));
    }

    // Append informational hints.
    for h in &outcome.hints {
        lines.push(format!("hint: {h}"));
    }

    let message = lines.join("\n");

    HelperResponse::Success {
        message,
        exit_code: outcome.exit_code,
    }
}

// ─── Version compatibility ───────────────────────────────────────────────────

// ─── OsbaseList dispatch ─────────────────────────────────────────────────────

fn dispatch_osbase_list() -> HelperResponse {
    use crate::osbase_install::list_scenarios;

    match list_scenarios() {
        Ok(names) => HelperResponse::Success {
            message: names.join("\n"),
            exit_code: 0,
        },
        Err(e) => HelperResponse::Error {
            code: "MANIFEST_ERROR".to_string(),
            message: format!("{e}"),
        },
    }
}

// ─── OsbaseUninstall dispatch ──────────────────────────────────────────────────

fn dispatch_osbase_uninstall(scenario: &str, dry_run: bool) -> HelperResponse {
    use crate::osbase_install::execute_uninstall;
    use crate::sandbox_manifest::SandboxManifest;

    // Pre-load manifest to know the package list for the response message.
    let packages_str = match SandboxManifest::load() {
        Ok(m) => m
            .find_scenario(scenario)
            .map(|c| c.packages.join(" "))
            .unwrap_or_default(),
        Err(_) => String::new(),
    };

    match execute_uninstall(scenario, dry_run) {
        Ok(_msg) => {
            // Build formatted progress lines for the CLI.
            let mut lines = Vec::new();
            if dry_run {
                if !packages_str.is_empty() {
                    lines.push(format!("[dry-run] would remove packages: {packages_str}"));
                }
                lines.push("[dry-run] no packages will be removed in dry-run mode".to_string());
            } else {
                if !packages_str.is_empty() {
                    lines.push(format!("removing packages: {packages_str}"));
                    lines.push("dnf remove completed (exit_code=0)".to_string());
                }
                lines.push("removed successfully".to_string());
            }
            HelperResponse::Success {
                message: lines.join("\n"),
                exit_code: 0,
            }
        }
        Err(e) => HelperResponse::Error {
            code: "EXECUTION_FAILED".to_string(),
            message: format!("{e}"),
        },
    }
}

/// Simple major-version compatibility check.
///
/// Both versions must share the same major version to be considered compatible.
fn is_compatible(cli_version: &str, helper_version: &str) -> bool {
    let cli_major = cli_version.split('.').next().unwrap_or("0");
    let helper_major = helper_version.split('.').next().unwrap_or("0");
    cli_major == helper_major
}

// ─── Request args extraction ─────────────────────────────────────────────────

/// Extract a short summary string from a request for audit logging.
fn request_args(req: &HelperRequest) -> String {
    match req {
        HelperRequest::OsbaseInstall {
            scenario, dry_run, ..
        } => {
            if *dry_run {
                format!("{scenario} (dry-run)")
            } else {
                scenario.clone()
            }
        }
        HelperRequest::OsbaseRemove { scenario, .. } => scenario.clone(),
        HelperRequest::OsbaseUninstall {
            scenario, dry_run, ..
        } => {
            if *dry_run {
                format!("{scenario} (dry-run)")
            } else {
                scenario.clone()
            }
        }
        HelperRequest::OsbaseList { filter } => filter.as_deref().unwrap_or("all").to_string(),
        HelperRequest::OsbaseStatus { scenario } => {
            scenario.as_deref().unwrap_or("all").to_string()
        }
        HelperRequest::OsbaseSetDefault { scenario } => scenario.clone(),
        HelperRequest::OsbaseDoctor { scenario, .. } => {
            scenario.as_deref().unwrap_or("all").to_string()
        }
        HelperRequest::WsCkptSnapshot { workspace } => workspace.clone(),
        HelperRequest::WsCkptRestore {
            workspace,
            checkpoint_id,
        } => {
            format!("{workspace}:{checkpoint_id}")
        }
        _ => String::new(),
    }
}

// ─── Audit log ───────────────────────────────────────────────────────────────

/// Append a JSONL audit record to the system-helper log.
///
/// Best-effort: failures are silently ignored (the daemon must not crash
/// because a log directory is temporarily unavailable).
fn write_audit_log(peer: &PeerCredential, op: &str, args: &str, exit_code: i32, duration_ms: u64) {
    let _ = fs::create_dir_all(AUDIT_LOG_DIR);

    let record = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "uid": peer.uid,
        "gid": peer.gid,
        "pid": peer.pid,
        "op": op,
        "args": args,
        "exit_code": exit_code,
        "duration_ms": duration_ms,
    });

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(AUDIT_LOG_PATH)
    {
        let _ = writeln!(f, "{record}");
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compatible_same_major() {
        assert!(is_compatible("0.5.0", "0.5.1"));
        assert!(is_compatible("1.0.0", "1.2.3"));
    }

    #[test]
    fn version_incompatible_different_major() {
        assert!(!is_compatible("0.5.0", "1.0.0"));
        assert!(!is_compatible("2.0.0", "1.0.0"));
    }

    #[test]
    fn dispatch_shutdown_requires_root() {
        let rate_limiter = Arc::new(Mutex::new(RateLimiter::new(30)));
        let last_op = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let start = Instant::now();

        // Non-root user tries shutdown.
        let peer = PeerCredential {
            uid: 1000,
            gid: 1000,
            pid: 1234,
        };
        let resp = dispatch(
            &HelperRequest::Shutdown,
            &peer,
            &rate_limiter,
            &last_op,
            &shutdown,
            "0.1.0",
            start,
        );
        assert!(
            matches!(resp, HelperResponse::Error { ref code, .. } if code == "PERMISSION_DENIED")
        );

        // Root user can shutdown.
        let root_peer = PeerCredential {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let resp = dispatch(
            &HelperRequest::Shutdown,
            &root_peer,
            &rate_limiter,
            &last_op,
            &shutdown,
            "0.1.0",
            start,
        );
        assert!(matches!(resp, HelperResponse::Success { .. }));
        assert!(shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn dispatch_system_status() {
        let rate_limiter = Arc::new(Mutex::new(RateLimiter::new(30)));
        let last_op = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let start = Instant::now();

        let peer = PeerCredential {
            uid: 1000,
            gid: 1000,
            pid: 5678,
        };
        let resp = dispatch(
            &HelperRequest::SystemStatus,
            &peer,
            &rate_limiter,
            &last_op,
            &shutdown,
            "0.1.0",
            start,
        );
        match resp {
            HelperResponse::Status {
                running, version, ..
            } => {
                assert!(running);
                assert_eq!(version, "0.1.0");
            }
            _ => panic!("expected Status response"),
        }
    }

    /// Verify that the helper response renders all five phases from
    /// outcome.phases (preflight, packages, services, verify, state),
    /// not from reconstructed metadata.
    #[test]
    fn helper_install_dryrun_surfaces_all_phases() {
        use crate::osbase_install::{OsbaseDomain, OsbaseInstallOutcome, PhaseResult, PhaseStatus};

        // Simulate a successful runc install outcome with all five phases.
        let outcome = OsbaseInstallOutcome {
            domain: OsbaseDomain::Sandbox,
            target: "runc".to_string(),
            phases: vec![
                PhaseResult {
                    name: "preflight".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("kernel 6.6.30 satisfies >=4.18".to_string()),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "packages".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("installed: runc containerd docker docker-client".to_string()),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "services".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("enabled: containerd, docker".to_string()),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "verify".to_string(),
                    status: PhaseStatus::Success,
                    message: Some(
                        "all checks passed: runc --version, systemctl is-active containerd, docker version, docker info"
                            .to_string(),
                    ),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "state".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("sandbox-runc recorded in /var/lib/anolisa/installed.toml".to_string()),
                    duration_ms: None,
                },
            ],
            exit_code: 0,
            warnings: vec![],
            hints: vec!["optional packages available: nerdctl".to_string()],
        };

        let resp = super::format_outcome_response(outcome);

        match resp {
            HelperResponse::Success { message, exit_code } => {
                assert_eq!(exit_code, 0);
                // All five phases must appear in the formatted output.
                assert!(
                    message.contains("preflight:"),
                    "missing preflight phase in helper output: {message}"
                );
                assert!(
                    message.contains("packages:"),
                    "missing packages phase in helper output: {message}"
                );
                assert!(
                    message.contains("services:"),
                    "missing services phase in helper output: {message}"
                );
                assert!(
                    message.contains("verify:"),
                    "missing verify phase in helper output: {message}"
                );
                assert!(
                    message.contains("state:"),
                    "missing state phase in helper output: {message}"
                );
                // Hints appear but NOT as warnings.
                assert!(
                    message.contains("hint: optional packages available: nerdctl"),
                    "missing hint line in helper output: {message}"
                );
                assert!(
                    !message.contains("warning:"),
                    "unexpected warning in clean install: {message}"
                );
            }
            other => panic!("expected Success, got: {other:?}"),
        }
    }

    /// Verify degraded verify phase shows as warning in helper output.
    #[test]
    fn helper_degraded_verify_shows_warning() {
        use crate::osbase_install::{OsbaseDomain, OsbaseInstallOutcome, PhaseResult, PhaseStatus};

        let outcome = OsbaseInstallOutcome {
            domain: OsbaseDomain::Sandbox,
            target: "runc".to_string(),
            phases: vec![
                PhaseResult {
                    name: "preflight".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("ok".to_string()),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "packages".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("ok".to_string()),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "services".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("ok".to_string()),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "verify".to_string(),
                    status: PhaseStatus::Degraded,
                    message: Some("docker info failed (exit 1)".to_string()),
                    duration_ms: None,
                },
                PhaseResult {
                    name: "state".to_string(),
                    status: PhaseStatus::Success,
                    message: Some("recorded".to_string()),
                    duration_ms: None,
                },
            ],
            exit_code: 2,
            warnings: vec!["verify degraded: docker info failed (exit 1)".to_string()],
            hints: vec![],
        };

        let resp = super::format_outcome_response(outcome);

        match resp {
            HelperResponse::Success { message, exit_code } => {
                assert_eq!(exit_code, 2, "degraded should exit 2");
                assert!(
                    message.contains("verify: degraded"),
                    "verify phase should show 'degraded': {message}"
                );
                assert!(
                    message.contains("warning: verify degraded"),
                    "warning line expected: {message}"
                );
            }
            other => panic!("expected Success, got: {other:?}"),
        }
    }
}
