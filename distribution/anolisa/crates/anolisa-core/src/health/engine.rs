//! Health-check execution engine.
//!
//! Single source of truth for "did this install actually come up?". The
//! engine is consumed by enable's `[7] Health Check` step today and is
//! shaped so `status` and `doctor` can reuse it. Every probe is bounded:
//! paths must resolve under an ANOLISA-owned root, command argv runs
//! *without* a shell, and spawned children are killed past a timeout — a
//! hostile manifest must not be able to read `/etc/shadow`, run a pipeline,
//! or hang the caller.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anolisa_platform::fs_layout::FsLayout;

use super::spec::{CheckOutcome, CheckSpec, CheckStatus};
use crate::manifest::{ServiceScope, ServiceSpec, declared_unit_scope};
use crate::path_safety::{PathBoundaryError, validate_owned_path};
use crate::service::{ServiceManager, ServiceState};

/// Default per-process probe timeout. Generous for `--version`/`--help`
/// smoke probes while keeping a hostile or wedged child bounded.
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll cadence for the spawn-wait loop — sub-second responsiveness without
/// burning a core.
const PROBE_POLL: Duration = Duration::from_millis(25);

/// Glyphs that turn an argv token into a shell expression. Probes never run
/// through `sh -c`, so a token requiring shell interpretation is, by
/// definition, not a valid probe and is refused.
const SHELL_METACHARS: &[char] = &[
    ';', '|', '&', '>', '<', '$', '`', '\\', '{', '}', '(', ')', '*', '?', '~', '!', '\n', '\r',
    '\'', '"',
];

/// Execution context for [`run_check`].
pub struct CheckEnv<'a> {
    /// Layout that bounds probe paths and expands `{bindir}`-style templates.
    pub layout: &'a FsLayout,
    /// When true, every node short-circuits to [`CheckStatus::Skipped`] and
    /// no process is spawned — owners never handle a dry-run flag themselves.
    pub dry_run: bool,
    /// Backends for `systemd_active` probes. `None` keeps those checks at
    /// [`CheckStatus::Unsupported`] — callers without service authority
    /// (or on hosts without one) simply omit it.
    pub service_probes: Option<ServiceProbes<'a>>,
}

/// Scope-routed backends for `systemd_active` probes.
///
/// A manifest may declare both system- and user-scope services and point
/// health checks at either, so a single manager cannot answer a whole spec
/// tree: each `systemd_active` leaf is routed to the manager owning its
/// unit's declared scope.
pub struct ServiceProbes<'a> {
    /// Backend answering system-scope units (`systemctl`).
    pub system: &'a dyn ServiceManager,
    /// Backend answering user-scope units (`systemctl --user`).
    pub user: &'a dyn ServiceManager,
    /// `[[component.services]]` declarations assigning each unit its scope;
    /// a unit with no covering declaration probes as system scope.
    pub declared: &'a [ServiceSpec],
}

impl ServiceProbes<'_> {
    fn manager_for(&self, unit: &str) -> &dyn ServiceManager {
        match declared_unit_scope(self.declared, unit) {
            ServiceScope::System => self.system,
            ServiceScope::User => self.user,
        }
    }
}

/// Run one (possibly aggregate) check spec and return a structured outcome.
///
/// Aggregates recurse, so a dry-run `all_of` yields a tree of `Skipped`
/// nodes (zero processes started) rather than a single opaque node.
pub fn run_check(spec: &CheckSpec, env: &CheckEnv<'_>) -> CheckOutcome {
    match spec {
        CheckSpec::AllOf { checks, .. } => {
            let children: Vec<CheckOutcome> = checks.iter().map(|c| run_check(c, env)).collect();
            let status = all_of_status(&children);
            CheckOutcome {
                spec_label: format!("all_of ({} checks)", checks.len()),
                status,
                detail: None,
                children,
            }
        }
        CheckSpec::AnyOf { checks, .. } => {
            let children: Vec<CheckOutcome> = checks.iter().map(|c| run_check(c, env)).collect();
            let status = any_of_status(&children);
            CheckOutcome {
                spec_label: format!("any_of ({} checks)", checks.len()),
                status,
                detail: None,
                children,
            }
        }
        leaf => {
            if env.dry_run {
                return CheckOutcome::leaf(label_for(leaf), CheckStatus::Skipped, None);
            }
            run_leaf(leaf, env)
        }
    }
}

/// `all_of`: any failure fails the group; all-ok passes; all-skipped (dry-run)
/// stays skipped; any remaining gap (e.g. an unsupported child) downgrades to
/// `Unsupported` because the group could not be fully verified.
fn all_of_status(children: &[CheckOutcome]) -> CheckStatus {
    if children.iter().any(|c| c.status == CheckStatus::Failed) {
        CheckStatus::Failed
    } else if children.iter().all(|c| c.status == CheckStatus::Ok) {
        CheckStatus::Ok
    } else if children.iter().all(|c| c.status == CheckStatus::Skipped) {
        CheckStatus::Skipped
    } else {
        CheckStatus::Unsupported
    }
}

/// `any_of`: one ok passes; otherwise all-skipped stays skipped, any failure
/// fails, and only-unsupported reports unsupported.
fn any_of_status(children: &[CheckOutcome]) -> CheckStatus {
    if children.iter().any(|c| c.status == CheckStatus::Ok) {
        CheckStatus::Ok
    } else if children.iter().all(|c| c.status == CheckStatus::Skipped) {
        CheckStatus::Skipped
    } else if children.iter().any(|c| c.status == CheckStatus::Failed) {
        CheckStatus::Failed
    } else {
        CheckStatus::Unsupported
    }
}

/// Dispatch a leaf check (callers guarantee `spec` is not an aggregate and
/// dry-run was already handled).
fn run_leaf(spec: &CheckSpec, env: &CheckEnv<'_>) -> CheckOutcome {
    match spec {
        CheckSpec::BinaryVersion {
            binary,
            expect_pattern,
            timeout_secs,
        } => check_binary(
            env,
            "binary_version",
            binary,
            &["--version"],
            expect_pattern.as_deref(),
            *timeout_secs,
        ),
        CheckSpec::BinaryHelp {
            binary,
            timeout_secs,
        } => check_binary(env, "binary_help", binary, &["--help"], None, *timeout_secs),
        CheckSpec::FileExists { path, mode, .. } => check_file_exists(env, path, mode.as_deref()),
        CheckSpec::Command {
            argv,
            expect_exit_code,
        } => check_command(env, argv, *expect_exit_code),
        CheckSpec::SystemdActive { service } => match &env.service_probes {
            Some(probes) => check_systemd_active(probes.manager_for(service), service),
            None => CheckOutcome::leaf(
                label_for(spec),
                CheckStatus::Unsupported,
                Some("no service manager available for this probe".to_string()),
            ),
        },
        // v1 stubs: interface frozen, execution deferred to the owning slice.
        CheckSpec::PortListen { .. }
        | CheckSpec::HttpGet { .. }
        | CheckSpec::BinaryCapabilities { .. } => CheckOutcome::leaf(
            label_for(spec),
            CheckStatus::Unsupported,
            Some("check type not yet implemented in this build".to_string()),
        ),
        // Aggregates are handled by `run_check`; never reached here.
        CheckSpec::AllOf { .. } | CheckSpec::AnyOf { .. } => {
            CheckOutcome::leaf(label_for(spec), CheckStatus::Unsupported, None)
        }
    }
}

/// Spawn `<binary> <args>` under the owned-root guard and resolve to
/// ok/failed/unsupported. Optionally require `expect_pattern` in stdout.
fn check_binary(
    env: &CheckEnv<'_>,
    kind: &str,
    binary: &str,
    args: &[&str],
    expect_pattern: Option<&str>,
    timeout_secs: Option<u64>,
) -> CheckOutcome {
    let expanded = expand_placeholders(binary, env.layout);
    let label = format!("{kind} binary={expanded}");
    let exe = Path::new(&expanded);
    if let Some(reason) = reject_unowned_executable(env.layout, exe) {
        return CheckOutcome::leaf(label, CheckStatus::Unsupported, Some(reason));
    }
    let timeout = timeout_secs
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_PROBE_TIMEOUT);
    match spawn_and_wait(exe, args, expect_pattern, timeout) {
        SpawnResult::Exited {
            success,
            code,
            pattern_seen,
        } => {
            if !success {
                return CheckOutcome::leaf(
                    label,
                    CheckStatus::Failed,
                    Some(format!(
                        "`{expanded} {}` exited with status {code}",
                        args.join(" ")
                    )),
                );
            }
            if let Some(pattern) = expect_pattern
                && !pattern_seen
            {
                return CheckOutcome::leaf(
                    label,
                    CheckStatus::Failed,
                    Some(format!("version output did not contain '{pattern}'")),
                );
            }
            CheckOutcome::leaf(label, CheckStatus::Ok, None)
        }
        SpawnResult::Timeout => CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some(format!(
                "`{expanded}` exceeded {}s probe timeout",
                timeout.as_secs()
            )),
        ),
        SpawnResult::SpawnError(err) => CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some(format!("failed to spawn '{expanded}': {err}")),
        ),
    }
}

/// Existence (and optional mode) check for a regular file, with the
/// owned-root + symlink guards `status` already relied on.
fn check_file_exists(env: &CheckEnv<'_>, path: &str, mode: Option<&str>) -> CheckOutcome {
    let expanded = expand_placeholders(path, env.layout);
    let label = format!("file_exists path={expanded}");
    let target = Path::new(&expanded);
    if let Err(err) = validate_owned_path(env.layout, target) {
        return CheckOutcome::leaf(
            label,
            CheckStatus::Unsupported,
            Some(format!(
                "path '{expanded}' rejected: {}",
                boundary_reason(&err)
            )),
        );
    }
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => CheckOutcome::leaf(
            label,
            CheckStatus::Unsupported,
            Some(format!(
                "path '{expanded}' is a symlink — refusing to follow"
            )),
        ),
        Ok(meta) if !meta.file_type().is_file() => CheckOutcome::leaf(
            label,
            CheckStatus::Unsupported,
            Some(format!("path '{expanded}' is not a regular file")),
        ),
        Ok(meta) => {
            if let Some(want) = mode {
                match parse_octal_mode(want) {
                    Some(want_bits) => {
                        let actual = meta.permissions().mode() & 0o777;
                        if actual != want_bits {
                            return CheckOutcome::leaf(
                                label,
                                CheckStatus::Failed,
                                Some(format!("mode {actual:04o} != expected {want_bits:04o}")),
                            );
                        }
                    }
                    None => {
                        return CheckOutcome::leaf(
                            label,
                            CheckStatus::Unsupported,
                            Some(format!("invalid expected mode '{want}'")),
                        );
                    }
                }
            }
            CheckOutcome::leaf(label, CheckStatus::Ok, None)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some(format!("missing file: {expanded}")),
        ),
        Err(err) => CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some(format!("stat failed for '{expanded}': {err}")),
        ),
    }
}

/// `systemd_active` probe through the caller-supplied [`ServiceManager`].
/// The mapping mirrors the manager's vocabulary: only `Active` proves
/// health; a missing or stopped unit is a failure; a backend that refuses
/// the host (user mode, containers, non-Linux) is `Unsupported` because
/// nothing was proven either way.
fn check_systemd_active(manager: &dyn ServiceManager, service: &str) -> CheckOutcome {
    let label = format!("systemd_active service={service}");
    if !manager.supported() {
        let reason = manager
            .unsupported_reason()
            .unwrap_or("service manager not supported in this environment")
            .to_string();
        return CheckOutcome::leaf(label, CheckStatus::Unsupported, Some(reason));
    }
    match manager.probe_service(service) {
        Ok(outcome) => match outcome.state {
            ServiceState::Active => CheckOutcome::leaf(
                label,
                CheckStatus::Ok,
                Some(format!("unit '{service}' is active")),
            ),
            ServiceState::NotSupported => CheckOutcome::leaf(
                label,
                CheckStatus::Unsupported,
                Some(if outcome.message.is_empty() {
                    "service manager unsupported".to_string()
                } else {
                    outcome.message.clone()
                }),
            ),
            other => CheckOutcome::leaf(
                label,
                CheckStatus::Failed,
                Some(format!("unit '{service}' state '{}'", other.as_str())),
            ),
        },
        Err(err) => CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some(format!("probe failed for '{service}': {err}")),
        ),
    }
}

/// Explicit-argv command probe. No shell is involved, but every token is
/// still placeholder-expanded and screened for shell metacharacters as
/// defense in depth, and `argv[0]` must be an owned-root executable.
fn check_command(
    env: &CheckEnv<'_>,
    argv: &[String],
    expect_exit_code: Option<i32>,
) -> CheckOutcome {
    let label = format!("command argv={}", argv.join(" "));
    let Some((exe_raw, rest)) = argv.split_first() else {
        return CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some("command argv is empty".to_string()),
        );
    };
    let expanded: Vec<String> = std::iter::once(exe_raw)
        .chain(rest)
        .map(|t| expand_placeholders(t, env.layout))
        .collect();
    if let Some(meta) = expanded
        .iter()
        .flat_map(|t| t.chars())
        .find(|c| SHELL_METACHARS.contains(c))
    {
        return CheckOutcome::leaf(
            label,
            CheckStatus::Unsupported,
            Some(format!(
                "argv contains shell metacharacter '{meta}' — commands run without a shell"
            )),
        );
    }
    let exe = Path::new(&expanded[0]);
    if let Some(reason) = reject_unowned_executable(env.layout, exe) {
        return CheckOutcome::leaf(label, CheckStatus::Unsupported, Some(reason));
    }
    let args: Vec<&str> = expanded[1..].iter().map(String::as_str).collect();
    match spawn_and_wait(exe, &args, None, DEFAULT_PROBE_TIMEOUT) {
        SpawnResult::Exited { code, .. } => {
            let want = expect_exit_code.unwrap_or(0);
            if code == want {
                CheckOutcome::leaf(label, CheckStatus::Ok, None)
            } else {
                CheckOutcome::leaf(
                    label,
                    CheckStatus::Failed,
                    Some(format!("exit code {code} != expected {want}")),
                )
            }
        }
        SpawnResult::Timeout => CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some(format!(
                "exceeded {}s probe timeout",
                DEFAULT_PROBE_TIMEOUT.as_secs()
            )),
        ),
        SpawnResult::SpawnError(err) => CheckOutcome::leaf(
            label,
            CheckStatus::Failed,
            Some(format!("failed to spawn: {err}")),
        ),
    }
}

/// Outcome of [`spawn_and_wait`].
enum SpawnResult {
    /// Child exited; `code` is the exit status (`-1` for signal termination).
    Exited {
        success: bool,
        code: i32,
        /// Whether the probe's stdout contained `expect_pattern` before the
        /// deadline (`false` when no pattern was requested).
        pattern_seen: bool,
    },
    /// Child exceeded the timeout and was killed.
    Timeout,
    /// Spawn itself failed (missing/non-executable binary).
    SpawnError(std::io::Error),
}

/// Spawn `exe args`, poll until exit or timeout, and (when `expect_pattern`
/// is set) match the probe's stdout against it as the output arrives.
/// stdin/stderr are always null; stdout is null unless a pattern is
/// requested.
fn spawn_and_wait(
    exe: &Path,
    args: &[&str],
    expect_pattern: Option<&str>,
    timeout: Duration,
) -> SpawnResult {
    let stdout_cfg = if expect_pattern.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    // spawn_retry_etxtbsy: health probes exec binaries ANOLISA installed;
    // a concurrent fork elsewhere can hold the write descriptor for a
    // moment and fail exec with ETXTBSY.
    let mut child = match crate::process::spawn_retry_etxtbsy(
        Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout_cfg)
            .stderr(Stdio::null()),
    ) {
        Ok(c) => c,
        Err(err) => return SpawnResult::SpawnError(err),
    };
    // Drain the piped stdout while polling for exit: a probe that writes
    // more than the OS pipe buffer (64 KiB on Linux) would otherwise block
    // on write, never exit, and be killed at the timeout even though it was
    // healthy — the deadlock that `adapter::manager::run_capture`
    // documents for the same pattern. The read end is non-blocking and the
    // poll loop drains it itself, matching the pattern incrementally: memory
    // stays O(pattern) no matter how much the probe writes, and the drain
    // never outlives the deadline when a grandchild holds the pipe.
    let mut stdout_pipe = child.stdout.take();
    let mut scanner = expect_pattern.map(PatternScanner::new);
    if let Some(pipe) = stdout_pipe.as_ref()
        && let Err(err) = set_nonblocking(pipe)
    {
        let _ = child.kill();
        let _ = child.wait();
        return SpawnResult::SpawnError(err);
    }
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Collect what the probe wrote before exiting. A writer
                // that outlives the probe gets the rest of the deadline to
                // produce the pattern, never more: the pipe is closed when
                // the deadline passes or every write end is gone.
                let mut eof = drain_pipe(&mut stdout_pipe, &mut scanner);
                while !eof
                    && scanner.as_ref().is_some_and(|scanner| !scanner.found())
                    && started.elapsed() <= timeout
                {
                    std::thread::sleep(PROBE_POLL);
                    eof = drain_pipe(&mut stdout_pipe, &mut scanner);
                }
                drop(stdout_pipe.take());
                return SpawnResult::Exited {
                    success: status.success(),
                    code: status.code().unwrap_or(-1),
                    pattern_seen: scanner.as_ref().is_some_and(PatternScanner::found),
                };
            }
            Ok(None) => {
                drain_pipe(&mut stdout_pipe, &mut scanner);
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Close the read end instead of waiting for EOF: a
                    // grandchild holding the pipe can neither delay the
                    // timeout result nor keep a drain reading after it.
                    drop(stdout_pipe.take());
                    return SpawnResult::Timeout;
                }
                std::thread::sleep(PROBE_POLL);
            }
            Err(err) => return SpawnResult::SpawnError(err),
        }
    }
}

/// Upper bound on bytes the stdout pipe may deliver per poll iteration, so
/// a probe that writes without end cannot starve the exit/timeout checks.
const DRAIN_BUDGET_PER_POLL: usize = 1 << 20;

/// Switch a pipe read end to non-blocking mode so the poll loop can drain
/// it in place instead of parking a helper thread on it.
fn set_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    nix::fcntl::fcntl(
        pipe.as_raw_fd(),
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .map(|_| ())
    .map_err(std::io::Error::from)
}

/// Feed one currently-readable burst of `pipe` to `scanner`, returning
/// whether the pipe has reached EOF. The pipe must already be
/// non-blocking; callers own that setup so this helper can never park the
/// caller's poll loop on a stalled writer.
fn drain_pipe(pipe: &mut Option<impl Read>, scanner: &mut Option<PatternScanner<'_>>) -> bool {
    let Some(pipe) = pipe.as_mut() else {
        return true;
    };
    let mut budget = DRAIN_BUDGET_PER_POLL;
    let mut chunk = [0u8; 8192];
    while budget > 0 {
        match pipe.read(&mut chunk) {
            Ok(0) => return true,
            Ok(read) => {
                if let Some(scanner) = scanner.as_mut() {
                    scanner.push(&chunk[..read]);
                }
                budget = budget.saturating_sub(read);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return false,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return true,
        }
    }
    false
}

/// Incremental substring matcher for a probe's stdout. Between chunks it
/// retains only the last `pattern.len() - 1` bytes, so a match that
/// straddles a read boundary is still found while memory stays O(pattern)
/// no matter how much output the probe produces.
struct PatternScanner<'a> {
    pattern: &'a [u8],
    tail: Vec<u8>,
    seen: bool,
}

impl<'a> PatternScanner<'a> {
    fn new(pattern: &'a str) -> Self {
        Self {
            pattern: pattern.as_bytes(),
            tail: Vec::new(),
            seen: pattern.is_empty(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.seen {
            return;
        }
        let mut window = std::mem::take(&mut self.tail);
        window.extend_from_slice(chunk);
        self.seen = window
            .windows(self.pattern.len())
            .any(|candidate| candidate == self.pattern);
        if !self.seen {
            let keep = self.pattern.len() - 1;
            if window.len() > keep {
                window.drain(..window.len() - keep);
            }
            self.tail = window;
        }
    }

    fn found(&self) -> bool {
        self.seen
    }
}

/// Reject an executable that is not an absolute path under an ANOLISA-owned
/// root. Returns the rejection reason, or `None` when the path is acceptable.
fn reject_unowned_executable(layout: &FsLayout, exe: &Path) -> Option<String> {
    if !exe.is_absolute() {
        return Some(format!(
            "executable '{}' is not absolute — declare the full `{{bindir}}/...` path",
            exe.display()
        ));
    }
    match validate_owned_path(layout, exe) {
        Ok(()) => None,
        Err(err) => Some(format!(
            "executable '{}' rejected: {}",
            exe.display(),
            boundary_reason(&err)
        )),
    }
}

/// Human-readable rendering of a path-boundary rejection.
fn boundary_reason(err: &PathBoundaryError) -> String {
    match err {
        PathBoundaryError::External { path } => {
            format!("'{}' is outside ANOLISA-owned roots", path.display())
        }
        PathBoundaryError::Traversal { path } => {
            format!("'{}' contains '.' or '..' segments", path.display())
        }
    }
}

/// Expand the FHS / file-hierarchy layout placeholders a manifest may use in a probe path.
/// The minimal-schema names (`{sysconfdir}`/`{sharedir}`) and the legacy ones
/// (`{etcdir}`/`{datadir}`) both resolve to the same roots during the
/// additive-compat window.
fn expand_placeholders(input: &str, layout: &FsLayout) -> String {
    let bin = layout.bin_dir.display().to_string();
    let libexec = layout.libexec_dir.display().to_string();
    let lib = layout.lib_dir.display().to_string();
    let data = layout.datadir.display().to_string();
    let etc = layout.etc_dir.display().to_string();
    let state = layout.state_dir.display().to_string();
    let cache = layout.cache_dir.display().to_string();
    let log = layout.log_dir.display().to_string();
    input
        .replace("{bindir}", &bin)
        .replace("{libexecdir}", &libexec)
        .replace("{libdir}", &lib)
        .replace("{sharedir}", &data)
        .replace("{datadir}", &data)
        .replace("{sysconfdir}", &etc)
        .replace("{etcdir}", &etc)
        .replace("{statedir}", &state)
        .replace("{cachedir}", &cache)
        .replace("{logdir}", &log)
}

/// Parse an octal mode string (`"0755"`, `"755"`) into its low 12 bits.
fn parse_octal_mode(raw: &str) -> Option<u32> {
    u32::from_str_radix(raw.trim_start_matches("0o"), 8)
        .ok()
        .map(|m| m & 0o7777)
}

/// One-line label used when no richer context is available (stubs, dry-run).
fn label_for(spec: &CheckSpec) -> String {
    match spec {
        CheckSpec::BinaryVersion { binary, .. } => format!("binary_version binary={binary}"),
        CheckSpec::BinaryHelp { binary, .. } => format!("binary_help binary={binary}"),
        CheckSpec::SystemdActive { service } => format!("systemd_active service={service}"),
        CheckSpec::FileExists { path, .. } => format!("file_exists path={path}"),
        CheckSpec::PortListen { port, .. } => format!("port_listen port={port}"),
        CheckSpec::HttpGet { url, .. } => format!("http_get url={url}"),
        CheckSpec::BinaryCapabilities { binary, .. } => {
            format!("binary_capabilities binary={binary}")
        }
        CheckSpec::Command { argv, .. } => format!("command argv={}", argv.join(" ")),
        CheckSpec::AllOf { checks, .. } => format!("all_of ({} checks)", checks.len()),
        CheckSpec::AnyOf { checks, .. } => format!("any_of ({} checks)", checks.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn layout_for(home: &Path) -> FsLayout {
        FsLayout::user_with_overrides(home.to_path_buf(), None, None, None, None, None)
    }

    /// Write a 0755 shell script under `dir` and return its absolute path.
    fn write_exec(dir: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(dir).expect("mkdir");
        let path = dir.join(name);
        fs::write(&path, body).expect("write script");
        let mut perms = fs::metadata(&path).expect("stat").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[test]
    fn binary_version_ok_for_owned_executable() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let exe = write_exec(
            &layout.bin_dir,
            "tool",
            "#!/bin/sh\necho 'tool 1.2.3'\nexit 0\n",
        );
        let spec = CheckSpec::BinaryVersion {
            binary: exe.display().to_string(),
            expect_pattern: None,
            timeout_secs: None,
        };
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Ok, "detail={:?}", out.detail);
    }

    #[test]
    fn binary_version_missing_binary_fails_with_path_in_detail() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let missing = layout.bin_dir.join("absent");
        let spec = CheckSpec::BinaryVersion {
            binary: missing.display().to_string(),
            expect_pattern: None,
            timeout_secs: None,
        };
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Failed);
        assert!(out.detail.unwrap().contains("absent"));
    }

    #[test]
    fn binary_version_expect_pattern_mismatch_fails() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let exe = write_exec(&layout.bin_dir, "tool", "#!/bin/sh\necho 'tool 9.9.9'\n");
        let spec = CheckSpec::BinaryVersion {
            binary: exe.display().to_string(),
            expect_pattern: Some("1.2.3".to_string()),
            timeout_secs: None,
        };
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Failed);
    }

    /// A probe whose stdout exceeds the OS pipe buffer (64 KiB on Linux)
    /// must still be able to exit: the spawner drains the piped stdout
    /// while polling, so the child never blocks on write. Without the
    /// drain the child stalls, the poll loop never sees an exit, and a
    /// healthy fast probe is killed at the timeout.
    #[test]
    fn binary_version_stdout_larger_than_the_pipe_buffer_still_matches() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let exe = write_exec(
            &layout.bin_dir,
            "tool",
            "#!/bin/sh\ndd if=/dev/zero bs=65536 count=8\necho 'tool 1.2.3'\nexit 0\n",
        );
        let spec = CheckSpec::BinaryVersion {
            binary: exe.display().to_string(),
            expect_pattern: Some("tool 1.2.3".to_string()),
            timeout_secs: Some(2),
        };
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(
            out.status,
            CheckStatus::Ok,
            "chatty probe must not be killed: {:?}",
            out.detail
        );
    }

    /// A writer that outlives the probe must not hold the check open: the
    /// engine drains what the probe wrote, gives a lingering writer the
    /// rest of the deadline, then closes the pipe. Before the drain was
    /// bounded, the join waited for EOF — for as long as the writer lived.
    #[test]
    fn probe_outliving_writer_does_not_hold_the_check_open() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let home = tempdir().expect("tempdir");
            let layout = layout_for(home.path());
            // The background sleep inherits stdout and holds the pipe open
            // for 30 s; the probe itself prints nothing and exits at once.
            let exe = write_exec(&layout.bin_dir, "tool", "#!/bin/sh\n(sleep 30) &\nexit 0\n");
            let spec = CheckSpec::BinaryVersion {
                binary: exe.display().to_string(),
                expect_pattern: Some("tool 1.2.3".to_string()),
                timeout_secs: Some(1),
            };
            let out = run_check(
                &spec,
                &CheckEnv {
                    layout: &layout,
                    dry_run: false,
                    service_probes: None,
                },
            );
            tx.send((out.status, out.detail)).ok();
        });
        let (status, detail) = rx
            .recv_timeout(Duration::from_millis(1500))
            .expect("the check must not wait for a writer outliving the probe");
        assert_eq!(status, CheckStatus::Failed, "detail: {detail:?}");
        assert!(
            detail.unwrap_or_default().contains("did not contain"),
            "the missing pattern must be reported, not a timeout"
        );
    }

    /// After a timeout with a writer that keeps going, the engine must not
    /// keep a detached drain accumulating the probe's stdout: it used to
    /// retain every byte for as long as the writer lived.
    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_with_a_never_ending_writer_keeps_memory_bounded() {
        fn rss_kib() -> u64 {
            let status = std::fs::read_to_string("/proc/self/status").expect("read status");
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
                .expect("parse VmRSS")
        }

        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        // The writer outlives the probe the timeout kills; it stops early
        // on EPIPE once the engine closes the pipe, and is capped at
        // 256 MiB so a retained-drain regression cannot exhaust the test
        // host.
        let exe = write_exec(
            &layout.bin_dir,
            "tool",
            "#!/bin/sh\n(\n  i=0\n  while [ \"$i\" -lt 32 ]; do\n    dd if=/dev/zero bs=65536 count=128 || exit 0\n    i=$((i + 1))\n  done\n) &\nsleep 30\n",
        );
        let spec = CheckSpec::BinaryVersion {
            binary: exe.display().to_string(),
            expect_pattern: Some("tool 1.2.3".to_string()),
            timeout_secs: Some(1),
        };
        let before = rss_kib();
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Failed);
        assert!(
            out.detail.unwrap_or_default().contains("exceeded"),
            "the timeout must be reported"
        );
        // Give a retained drain (the old behaviour) time to accumulate.
        std::thread::sleep(Duration::from_millis(500));
        let after = rss_kib();
        assert!(
            after <= before + 96 * 1024,
            "stdout drain must stay bounded after a timeout: {before} KiB -> {after} KiB RSS"
        );
    }

    #[test]
    fn file_exists_ok_and_missing() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let present = write_exec(&layout.bin_dir, "present", "x");
        let ok = run_check(
            &CheckSpec::FileExists {
                path: present.display().to_string(),
                mode: None,
                owner: None,
            },
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(ok.status, CheckStatus::Ok);

        let missing = layout.bin_dir.join("nope");
        let out = run_check(
            &CheckSpec::FileExists {
                path: missing.display().to_string(),
                mode: None,
                owner: None,
            },
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Failed);
    }

    #[test]
    fn command_rejects_external_executable() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let out = run_check(
            &CheckSpec::Command {
                argv: vec!["/usr/bin/true".to_string()],
                expect_exit_code: None,
            },
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(
            out.status,
            CheckStatus::Unsupported,
            "detail={:?}",
            out.detail
        );
    }

    #[test]
    fn command_rejects_shell_metacharacters() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let exe = write_exec(&layout.bin_dir, "t", "#!/bin/sh\n");
        let out = run_check(
            &CheckSpec::Command {
                argv: vec![exe.display().to_string(), "a; rm -rf /".to_string()],
                expect_exit_code: None,
            },
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Unsupported);
    }

    #[test]
    fn all_of_fails_if_any_child_fails_and_keeps_children() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let present = write_exec(&layout.bin_dir, "present", "x");
        let spec = CheckSpec::AllOf {
            checks: vec![
                CheckSpec::FileExists {
                    path: present.display().to_string(),
                    mode: None,
                    owner: None,
                },
                CheckSpec::FileExists {
                    path: layout.bin_dir.join("gone").display().to_string(),
                    mode: None,
                    owner: None,
                },
            ],
            timeout_secs: None,
        };
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Failed);
        assert_eq!(out.children.len(), 2);
        assert_eq!(out.children[0].status, CheckStatus::Ok);
        assert_eq!(out.children[1].status, CheckStatus::Failed);
    }

    #[test]
    fn dry_run_skips_all_nodes_and_starts_no_process() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let marker = home.path().join("ran.marker");
        let exe = write_exec(
            &layout.bin_dir,
            "tool",
            &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        );
        let spec = CheckSpec::AllOf {
            checks: vec![CheckSpec::BinaryVersion {
                binary: exe.display().to_string(),
                expect_pattern: None,
                timeout_secs: None,
            }],
            timeout_secs: None,
        };
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: true,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Skipped);
        assert_eq!(out.children[0].status, CheckStatus::Skipped);
        assert!(!marker.exists(), "dry-run must not spawn the probe binary");
    }

    #[test]
    fn systemd_active_is_unsupported_without_a_manager() {
        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let out = run_check(
            &CheckSpec::SystemdActive {
                service: "anything.service".to_string(),
            },
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: None,
            },
        );
        assert_eq!(out.status, CheckStatus::Unsupported);
    }

    #[test]
    fn systemd_active_probes_through_the_service_manager() {
        use crate::service::{FakeServiceManager, ServiceOp, ServiceState};

        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let manager = FakeServiceManager::new();
        let spec = CheckSpec::SystemdActive {
            service: "agentsight.service".to_string(),
        };

        // Inactive unit → the check fails with the state in the detail.
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: Some(ServiceProbes {
                    system: &manager,
                    user: &manager,
                    declared: &[],
                }),
            },
        );
        assert_eq!(out.status, CheckStatus::Failed);
        assert!(out.detail.as_deref().unwrap_or("").contains("inactive"));

        // Active unit → ok.
        manager.set_state(ServiceState::Active);
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: Some(ServiceProbes {
                    system: &manager,
                    user: &manager,
                    declared: &[],
                }),
            },
        );
        assert_eq!(out.status, CheckStatus::Ok, "detail={:?}", out.detail);

        // Probe error → failed, never a panic.
        manager.fail(ServiceOp::Probe, "agentsight.service");
        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: Some(ServiceProbes {
                    system: &manager,
                    user: &manager,
                    declared: &[],
                }),
            },
        );
        assert_eq!(out.status, CheckStatus::Failed);
    }

    #[test]
    fn systemd_active_unsupported_backend_reports_unsupported() {
        use crate::service::NotSupportedServiceManager;

        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let manager =
            NotSupportedServiceManager::new("user mode has no systemd authority".to_string());
        let out = run_check(
            &CheckSpec::SystemdActive {
                service: "agentsight.service".to_string(),
            },
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: Some(ServiceProbes {
                    system: &manager,
                    user: &manager,
                    declared: &[],
                }),
            },
        );
        assert_eq!(out.status, CheckStatus::Unsupported);
    }

    #[test]
    fn systemd_active_routes_leaves_by_declared_scope() {
        use crate::manifest::{ServiceScope, ServiceSpec};
        use crate::service::{FakeServiceManager, ServiceOp, ServiceState};

        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let system = FakeServiceManager::new();
        system.set_state(ServiceState::Active);
        let user = FakeServiceManager::new();
        user.set_state(ServiceState::Active);
        // Only the user unit is declared; the daemon unit has no covering
        // declaration and must default to the system manager.
        let declared = vec![ServiceSpec {
            unit: "agentsight-user.service".to_string(),
            scope: ServiceScope::User,
            enable: true,
            start: true,
            instance: None,
        }];
        let spec = CheckSpec::AllOf {
            checks: vec![
                CheckSpec::SystemdActive {
                    service: "agentsight.service".to_string(),
                },
                CheckSpec::SystemdActive {
                    service: "agentsight-user.service".to_string(),
                },
            ],
            timeout_secs: None,
        };

        let out = run_check(
            &spec,
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: Some(ServiceProbes {
                    system: &system,
                    user: &user,
                    declared: &declared,
                }),
            },
        );

        assert_eq!(out.status, CheckStatus::Ok, "detail={:?}", out.detail);
        assert_eq!(
            system.calls(),
            vec![(ServiceOp::Probe, "agentsight.service".to_string())]
        );
        assert_eq!(
            user.calls(),
            vec![(ServiceOp::Probe, "agentsight-user.service".to_string())]
        );
    }

    #[test]
    fn systemd_active_routes_template_instances_to_the_declared_scope() {
        use crate::manifest::{ServiceScope, ServiceSpec};
        use crate::service::{FakeServiceManager, ServiceOp, ServiceState};

        let home = tempdir().expect("tempdir");
        let layout = layout_for(home.path());
        let system = FakeServiceManager::new();
        let user = FakeServiceManager::new();
        user.set_state(ServiceState::Active);
        let declared = vec![ServiceSpec {
            unit: "anolisa-memory@.service".to_string(),
            scope: ServiceScope::User,
            enable: true,
            start: true,
            instance: Some("%u".to_string()),
        }];

        let out = run_check(
            &CheckSpec::SystemdActive {
                service: "anolisa-memory@alice.service".to_string(),
            },
            &CheckEnv {
                layout: &layout,
                dry_run: false,
                service_probes: Some(ServiceProbes {
                    system: &system,
                    user: &user,
                    declared: &declared,
                }),
            },
        );

        assert_eq!(out.status, CheckStatus::Ok, "detail={:?}", out.detail);
        assert!(system.calls().is_empty(), "system manager must not be hit");
        assert_eq!(
            user.calls(),
            vec![(ServiceOp::Probe, "anolisa-memory@alice.service".to_string())]
        );
    }
}
