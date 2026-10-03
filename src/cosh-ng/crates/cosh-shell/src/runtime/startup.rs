use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{IsTerminal, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nix::pty::Winsize;
use wait_timeout::ChildExt;

use self::probe_outcome::record_probe_outcome;
use crate::diagnostics::health::{
    record_startup_health_recommendations, HealthFindingCategory, HealthScanReport, HealthSeverity,
};
use crate::raw_input::{PromptGhostCandidate, PromptGhostRoute};
use crate::recommendation::personal_context::discover_repo_context;
use crate::recommendation::personal_crypto::random_hex;
use crate::recommendation::personal_feedback::{FeedbackEvent, FrozenPromptBinding};
use crate::recommendation::personal_model::{
    CandidateEvidenceSummary, CandidateSource, ContextAffinity, FeedbackAction, ScopeKind,
    DISCLOSURE_VERSION,
};
use crate::recommendation::personal_planner::{
    plan_startup, HealthResolution, PlannerCandidate, PlannerContext,
};
use crate::runtime::cli_args::RawShellKind;
use crate::runtime::invocation::{
    classify_invocation, exec_shell, normalize_raw_invocation, Invocation,
};
use crate::runtime::prelude::*;
use crate::runtime::state::PendingInputGhostBinding;
use crate::shell_host::{LoginEffectGuard, LoginEffectSource};

const LOGO_LINES: &[&str] = &[
    "  ██████╗  ██████╗  ███████╗ ██╗  ██╗",
    " ██╔════╝ ██╔═══██╗ ██╔════╝ ██║  ██║",
    " ██║      ██║   ██║ ███████╗ ███████║",
    " ██║      ██║   ██║ ╚════██║ ██╔══██║",
    " ╚██████╗ ╚██████╔╝ ███████║ ██║  ██║",
    "  ╚═════╝  ╚═════╝  ╚══════╝ ╚═╝  ╚═╝",
];

const LOGO_COLORS: &[&str] = &[
    "\x1b[1;38;5;33m",
    "\x1b[1;38;5;33m",
    "\x1b[1;38;5;39m",
    "\x1b[1;38;5;39m",
    "\x1b[1;38;5;117m",
    "\x1b[1;38;5;117m",
];

const RESET: &str = "\x1b[0m";
const LOGO_MIN_WIDTH: u16 = 42;
const STARTUP_HEALTH_ROW_WAIT: Duration = Duration::from_millis(150);
const STARTUP_AUTH_HINT_WAIT: Duration = Duration::from_millis(150);
const BOOTSTRAP_PATH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_PATH_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

mod bash_capability;
mod descendants;
mod probe_outcome;
mod recommendations;
pub(crate) use bash_capability::{exported_bash_functions_posix_compatible, resolve_bash_for_r2};
#[cfg(test)]
use recommendations::{
    append_startup_auth_hint, plan_startup_for_render, record_visible_personal_impressions,
    visible_personal_candidates, write_startup_suggestion_card,
};
pub(crate) use recommendations::{
    render_pending_recommendation_notice, render_startup_banner, render_startup_health_banner,
};

fn restore_startup_prompt<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    crate::slash::prompt::write_shell_prompt(state, output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupSuggestionMode {
    Hidden,
    ReadOnly,
    Interactive,
}

fn startup_suggestion_mode(
    isolated: bool,
    term: Option<&str>,
    report: &HealthScanReport,
) -> StartupSuggestionMode {
    if !startup_suggestion_display_supported(isolated, term) {
        StartupSuggestionMode::Hidden
    } else if health_report_supports_interactive_suggestions(report) {
        StartupSuggestionMode::Interactive
    } else {
        StartupSuggestionMode::ReadOnly
    }
}

fn startup_suggestion_display_supported(isolated: bool, term: Option<&str>) -> bool {
    !isolated && !term.is_some_and(|term| term.eq_ignore_ascii_case("dumb"))
}

fn health_report_supports_interactive_suggestions(report: &HealthScanReport) -> bool {
    !report
        .findings
        .iter()
        .any(|finding| finding.category == HealthFindingCategory::CollectionGap)
        && !report.unavailable.iter().any(|item| {
            matches!(
                item.severity,
                HealthSeverity::Unavailable | HealthSeverity::Degraded
            )
        })
}

pub(crate) fn startup_banner_enabled() -> bool {
    match std::env::var("COSH_SHELL_STARTUP_BANNER") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "always"
        ),
        Err(_) => std::io::stdout().is_terminal(),
    }
}

struct StartupHookResult {
    summary: String,
    markdown: Option<String>,
}

fn evaluate_startup_hooks(cwd: &str, i18n: I18n) -> StartupHookResult {
    if !startup_hooks_enabled() {
        return StartupHookResult {
            summary: i18n.t(MessageId::StartupHooksNoneSummary).to_string(),
            markdown: None,
        };
    }

    let mut findings = Vec::new();
    let cwd_path = Path::new(cwd);
    if cwd_path.join("Cargo.toml").is_file() {
        findings.push(format!(
            "- {}",
            i18n.t(MessageId::StartupHooksRustProjectFinding)
        ));
    }

    if findings.is_empty() {
        findings.push(format!("- {}", i18n.t(MessageId::StartupHooksNoFindings)));
    }

    StartupHookResult {
        summary: i18n.t(MessageId::StartupHooksCompletedSummary).to_string(),
        markdown: Some(format!(
            "## {}\n\n{}\n\n{}",
            i18n.t(MessageId::StartupHooksFindingsHeading),
            findings.join("\n"),
            i18n.t(MessageId::StartupHooksReadOnlyNote)
        )),
    }
}

fn startup_hooks_enabled() -> bool {
    std::env::var("COSH_SHELL_STARTUP_HOOKS")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "builtin" | "built-in"
            )
        })
}

#[derive(Clone, Copy)]
struct BootstrapPathProbe {
    flags: &'static str,
    source: &'static str,
    io: BootstrapPathProbeIo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BootstrapPathProbeIo {
    Pipes,
    Pty,
}

const BASH_NON_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[
    BootstrapPathProbe {
        flags: "-ic",
        source: "Bash interactive startup",
        io: BootstrapPathProbeIo::Pipes,
    },
    BootstrapPathProbe {
        flags: "-lic",
        source: "Bash interactive login startup",
        io: BootstrapPathProbeIo::Pty,
    },
];
const BASH_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[BootstrapPathProbe {
    flags: "-lic",
    source: "Bash interactive login startup",
    io: BootstrapPathProbeIo::Pipes,
}];
const ZSH_NON_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[BootstrapPathProbe {
    flags: "-ic",
    source: "Zsh interactive startup",
    io: BootstrapPathProbeIo::Pipes,
}];
const ZSH_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[BootstrapPathProbe {
    flags: "-lic",
    source: "Zsh interactive login startup",
    io: BootstrapPathProbeIo::Pipes,
}];

#[derive(Debug)]
enum BootstrapPathProbeError {
    Containment(std::io::Error),
    Supervisor(std::io::Error),
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Read(std::io::Error),
    Failed(ExitStatus),
    TimedOut(Duration),
    OutputDidNotClose(Duration),
    OutputTooLarge,
    MissingMarker,
}

impl fmt::Display for BootstrapPathProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Containment(error) => {
                write!(formatter, "could not contain shell descendants: {error}")
            }
            Self::Supervisor(error) => {
                write!(formatter, "profile probe supervisor failed: {error}")
            }
            Self::Spawn(error) => write!(formatter, "could not start shell: {error}"),
            Self::Wait(error) => write!(formatter, "could not wait for shell: {error}"),
            Self::Read(error) => write!(formatter, "could not read shell output: {error}"),
            Self::Failed(status) => write!(formatter, "shell exited with {status}"),
            Self::TimedOut(timeout) => write!(formatter, "shell timed out after {timeout:?}"),
            Self::OutputDidNotClose(timeout) => {
                write!(formatter, "shell output did not close within {timeout:?}")
            }
            Self::OutputTooLarge => formatter.write_str("shell output exceeded the capture limit"),
            Self::MissingMarker => formatter.write_str("shell did not report PATH"),
        }
    }
}

fn bootstrap_path_probe_plan(
    shell_kind: &RawShellKind,
    login: bool,
    enabled: bool,
) -> Option<(&'static str, &'static [BootstrapPathProbe])> {
    if !enabled {
        return None;
    }
    match (shell_kind, login) {
        (RawShellKind::Bash, false) => Some(("bash", BASH_NON_LOGIN_PATH_PROBES)),
        (RawShellKind::Bash, true) => Some(("bash", BASH_LOGIN_PATH_PROBES)),
        (RawShellKind::Zsh, false) => Some(("zsh", ZSH_NON_LOGIN_PATH_PROBES)),
        (RawShellKind::Zsh, true) => Some(("zsh", ZSH_LOGIN_PATH_PROBES)),
        _ => None,
    }
}

fn bootstrap_path_command(shell: &str, flags: &str) -> Command {
    let mut command = Command::new(shell);
    command
        .arg(flags)
        .arg("printf '\\n__COSH_PATH_BEGIN__%s__COSH_PATH_END__\\n' \"$PATH\"")
        .env("COSH_SHELL_BOOTSTRAP_PATH", "0");
    command
}

fn run_bootstrap_path_probe(
    command: Command,
    timeout: Duration,
    io: BootstrapPathProbeIo,
    winsize: &Winsize,
) -> Result<String, BootstrapPathProbeError> {
    match io {
        BootstrapPathProbeIo::Pipes => {
            run_bootstrap_path_probe_direct(command, timeout, io, winsize, false)
        }
        BootstrapPathProbeIo::Pty => {
            descendants::run_supervised_profile_probe(command, None, timeout, winsize)
        }
    }
}

struct BootstrapPathProbeOutput {
    status: ExitStatus,
    streams: Vec<Vec<u8>>,
}

fn run_bootstrap_path_probe_direct(
    command: Command,
    timeout: Duration,
    io: BootstrapPathProbeIo,
    winsize: &Winsize,
    contain_descendants: bool,
) -> Result<String, BootstrapPathProbeError> {
    let output = execute_bootstrap_path_probe(command, timeout, io, winsize, contain_descendants)?;
    if !output.status.success() {
        return Err(BootstrapPathProbeError::Failed(output.status));
    }
    let bytes = output.streams.concat();
    let text = String::from_utf8_lossy(&bytes);
    extract_bootstrap_path(&text).ok_or(BootstrapPathProbeError::MissingMarker)
}

fn execute_bootstrap_path_probe(
    mut command: Command,
    timeout: Duration,
    io: BootstrapPathProbeIo,
    winsize: &Winsize,
    contain_descendants: bool,
) -> Result<BootstrapPathProbeOutput, BootstrapPathProbeError> {
    let deadline = Instant::now() + timeout;
    let descendants = if contain_descendants {
        Some(
            descendants::ProfileProbeDescendants::enter()
                .map_err(BootstrapPathProbeError::Containment)?,
        )
    } else {
        None
    };
    let spawned = match io {
        BootstrapPathProbeIo::Pipes => {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            unsafe {
                command.pre_exec(|| {
                    // Detach pipe-backed interactive probes from Cosh's
                    // controlling terminal so Bash cannot stop as a
                    // background process group while initializing job control.
                    if nix::libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            command.spawn().map(|mut child| {
                let stdout = drain_bootstrap_path_pipe(child.stdout.take());
                let stderr = drain_bootstrap_path_pipe(child.stderr.take());
                (child, vec![stdout, stderr])
            })
        }
        BootstrapPathProbeIo::Pty => {
            crate::shell_host::spawn_profile_probe_on_pty(command, winsize)
                .map(|(child, master)| (child, vec![drain_bootstrap_path_pty(master)]))
        }
    };
    let (mut child, output) = match spawned {
        Ok(spawned) => spawned,
        Err(error) => {
            descendants::cancel_pending(descendants)?;
            return Err(BootstrapPathProbeError::Spawn(error));
        }
    };
    let process_group = child.id();

    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => Ok(status),
        Ok(None) => {
            terminate_bootstrap_path_probe(&mut child, process_group);
            Err(BootstrapPathProbeError::TimedOut(timeout))
        }
        Err(error) => {
            terminate_bootstrap_path_probe(&mut child, process_group);
            Err(BootstrapPathProbeError::Wait(error))
        }
    };
    let status = descendants::finish_pending(descendants, status)?;
    let mut streams = Vec::with_capacity(output.len());
    for receiver in output {
        let stream = collect_bootstrap_path_pipe(receiver, deadline, timeout, process_group)?;
        if stream.len() > BOOTSTRAP_PATH_MAX_OUTPUT_BYTES {
            kill_bootstrap_path_process_group(process_group);
            return Err(BootstrapPathProbeError::OutputTooLarge);
        }
        streams.push(stream);
    }
    Ok(BootstrapPathProbeOutput { status, streams })
}

/// Runs the internal profile-probe supervisor before normal CLI dispatch.
pub(crate) fn run_profile_probe_helper_if_requested() -> Option<i32> {
    descendants::run_helper_if_requested()
}

fn drain_bootstrap_path_pipe<R: Read + Send + 'static>(
    pipe: Option<R>,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = match pipe {
            Some(source) => {
                let mut bytes = Vec::new();
                source
                    .take((BOOTSTRAP_PATH_MAX_OUTPUT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map(|_| bytes)
            }
            None => Ok(Vec::new()),
        };
        let _ = sender.send(result);
    });
    receiver
}

fn drain_bootstrap_path_pty(
    mut master: File,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        let result = loop {
            match master.read(&mut buffer) {
                Ok(0) => break Ok(bytes),
                Ok(count) => {
                    let remaining = BOOTSTRAP_PATH_MAX_OUTPUT_BYTES + 1 - bytes.len();
                    bytes.extend_from_slice(&buffer[..count.min(remaining)]);
                    if bytes.len() > BOOTSTRAP_PATH_MAX_OUTPUT_BYTES {
                        break Ok(bytes);
                    }
                }
                Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => break Ok(bytes),
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(result);
    });
    receiver
}

fn collect_bootstrap_path_pipe(
    receiver: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    deadline: Instant,
    timeout: Duration,
    process_group: u32,
) -> Result<Vec<u8>, BootstrapPathProbeError> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| {
            kill_bootstrap_path_process_group(process_group);
            BootstrapPathProbeError::OutputDidNotClose(timeout)
        })?
        .map_err(BootstrapPathProbeError::Read)
}

fn terminate_bootstrap_path_probe(child: &mut std::process::Child, process_group: u32) {
    kill_bootstrap_path_process_group(process_group);
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_bootstrap_path_process_group(process_group: u32) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    let _ = killpg(Pid::from_raw(process_group as i32), Signal::SIGKILL);
}

fn merge_bootstrap_paths(
    shell_kind: &RawShellKind,
    login: bool,
    discovered: &[Option<String>],
    current: &str,
) -> String {
    let non_login_path = discovered.first().and_then(Option::as_deref);
    let login_path = discovered.get(1).and_then(Option::as_deref);
    let mut paths = Vec::with_capacity(discovered.len() + 1);
    if matches!(shell_kind, RawShellKind::Bash) && !login {
        paths.extend(non_login_path);
    } else {
        paths.extend(discovered.iter().filter_map(Option::as_deref));
    }
    paths.push(current);
    let merged = merge_path_lists(&paths);

    let merged = if matches!(shell_kind, RawShellKind::Bash) && !login {
        login_path
            .map(|path| merge_path_additions_at_anchors(path, &merged))
            .unwrap_or(merged)
    } else {
        merged
    };

    // Append synthetic fallbacks only after resolving login PATH precedence.
    merge_path_lists(&[
        &merged,
        "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    ])
}

fn merge_path_additions_at_anchors(overlay: &str, base: &str) -> String {
    let mut merged = base
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let anchors = merged.iter().cloned().collect::<HashSet<_>>();
    let mut seen_additions = HashSet::new();
    let mut pending = Vec::new();
    let mut last_anchor = None;

    for entry in overlay.split(':').filter(|entry| !entry.is_empty()) {
        if anchors.contains(entry) {
            if !pending.is_empty() {
                if let Some(index) = merged.iter().position(|item| item == entry) {
                    merged.splice(index..index, pending.drain(..));
                }
            }
            last_anchor = Some(entry);
        } else if seen_additions.insert(entry) {
            pending.push(entry.to_string());
        }
    }

    if !pending.is_empty() {
        let index = last_anchor
            .and_then(|anchor| merged.iter().position(|item| item == anchor))
            .map_or(0, |index| index + 1);
        merged.splice(index..index, pending);
    }

    merged.join(":")
}

pub(crate) fn bootstrap_process_path_from_shell(
    shell_kind: &RawShellKind,
    login: bool,
    winsize: &Winsize,
    effects: &LoginEffectGuard,
) {
    let enabled = std::env::var("COSH_SHELL_BOOTSTRAP_PATH").as_deref() != Ok("0");
    let Some((shell, probes)) = bootstrap_path_probe_plan(shell_kind, login, enabled) else {
        return;
    };

    let mut discovered = Vec::with_capacity(probes.len());
    for probe in probes {
        // Each child imports only PATH; probe failures stay independently visible.
        let command = bootstrap_path_command(shell, probe.flags);
        let outcome =
            run_bootstrap_path_probe(command, BOOTSTRAP_PATH_PROBE_TIMEOUT, probe.io, winsize);
        record_probe_outcome(effects, LoginEffectSource::PathBootstrapProbe, &outcome);
        match outcome {
            Ok(path) => discovered.push(Some(path)),
            Err(error) => {
                eprintln!(
                    "cosh-shell: failed to discover PATH from {}: {error}",
                    probe.source
                );
                discovered.push(None);
            }
        }
    }
    if discovered.iter().all(Option::is_none) {
        return;
    }

    let current = std::env::var("PATH").unwrap_or_default();
    // Import login-specific additions around base anchors; the managed shell owns final ordering.
    let merged = merge_bootstrap_paths(shell_kind, login, &discovered, &current);
    if merged != current {
        std::env::set_var("PATH", merged);
    }
}

pub(crate) fn passthrough_non_interactive(args: &[String]) -> Option<i32> {
    // Documented `cosh-shell` extension: `-- <command> [args…]` executes the
    // command directly (no shell). The `/usr/bin/cosh` entry never reaches
    // this path; there `--` is handed to bash verbatim.
    if args.get(1).map(String::as_str) == Some("--") {
        let Some(command) = args.get(2) else {
            eprintln!("cosh-shell: missing command after --");
            return Some(2);
        };
        let status = Command::new(command)
            .args(&args[3..])
            .status()
            .map(passthrough_exit_code)
            .unwrap_or_else(|err| {
                let command = crate::evidence::redact_sensitive_text(command).0;
                let err = crate::evidence::redact_sensitive_text(&err.to_string()).0;
                eprintln!("cosh-shell: exec {command} failed: {err}");
                126
            });
        return Some(status);
    }

    let argv0 = OsString::from(args[0].as_str());
    let rest = args[1..].iter().map(OsString::from).collect::<Vec<_>>();
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    let stderr_tty = std::io::stderr().is_terminal();
    match classify_invocation(&argv0, &rest, stdin_tty, stdout_tty, stderr_tty) {
        Invocation::ExecShell(plan) => Some(exec_shell(plan)),
        Invocation::Tui(_) => None,
    }
}

fn passthrough_exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

pub(crate) fn passthrough_raw_non_interactive(args: &[String]) -> Option<i32> {
    let rest = args[1..].iter().map(OsString::from).collect::<Vec<_>>();
    let normalized = normalize_raw_invocation(&rest)?;
    // Leading `--`: same documented direct-exec extension as the bare
    // `cosh-shell` surface.
    if normalized.first().and_then(|arg| arg.to_str()) == Some("--") {
        let mut forwarded = vec![args[0].clone()];
        forwarded.extend(
            normalized
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        return passthrough_non_interactive(&forwarded);
    }
    // `raw` is an explicit TUI request, so classify argv with terminals assumed;
    // piped drivers must never be diverted away from an interactive session.
    let argv0 = OsString::from(args[0].as_str());
    match classify_invocation(&argv0, &normalized, true, true, true) {
        Invocation::ExecShell(plan) => Some(exec_shell(plan)),
        Invocation::Tui(_) => None,
    }
}

pub(crate) fn print_usage_help() {
    println!(
        "Usage: cosh-shell [OPTIONS]\n\
         \n\
         AI-augmented interactive shell wrapper.\n\
         \n\
         Modes:\n\
          raw [adapter] [--run]   Interactive mode with AI (adapters: fake, claude, co, qwen, cosh-core)\n\
          diagnostics export      Export a redacted diagnostic bundle\n\
           demo                    Demo with synthetic events\n\
         \n\
         Options:\n\
           -c <command>            Execute command and exit (passthrough to bash/zsh)\n\
           -- <command> [args...]   Execute command directly and exit\n\
           --shell <shell>         Use specified shell (bash, zsh) [default: bash]\n\
           --resume [session-id]   Open the session picker or resume a provider session\n\
           --isolated              Isolated mode: skip user rcfiles\n\
           --login, -l             Treat as login shell\n\
           --version               Print version\n\
           --help                  Print help"
    );
}

fn extract_bootstrap_path(text: &str) -> Option<String> {
    let start = text.rfind("__COSH_PATH_BEGIN__")? + "__COSH_PATH_BEGIN__".len();
    let rest = &text[start..];
    let end = rest.find("__COSH_PATH_END__")?;
    let path = rest[..end].trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn merge_path_lists(paths: &[&str]) -> String {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for path in paths {
        for item in path.split(':') {
            if item.is_empty() {
                continue;
            }
            if seen.insert(item.to_string()) {
                merged.push(item.to_string());
            }
        }
    }
    merged.join(":")
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
