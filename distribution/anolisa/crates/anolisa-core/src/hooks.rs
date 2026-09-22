//! Lifecycle hook runner.
//!
//! Components can declare per-phase scripts (`pre_enable`, `post_enable`,
//! `pre_disable`, `post_disable`, …) in their manifest. The runner is the
//! one place those scripts get executed; CLI handlers call into it during
//! the lifecycle phases instead of shelling out themselves so the
//! enforcement story stays uniform:
//!
//!   1. **Path-safety**: the script path is validated against
//!      [`crate::path_safety::validate_owned_path`]. A hook that lives
//!      outside ANOLISA's owned roots (`/etc/passwd-pre-enable.sh`, a
//!      symlink-into-`/etc`, etc.) is refused without ever running.
//!      Components cannot opt out of this guard — third-party packages
//!      ship hooks under `<datadir>/hooks/<component>/...`, declare them in
//!      `[[component.hooks]]`, and the runner resolves those declarations
//!      via [`resolve_manifest_hooks`] before spawning.
//!   2. **Execution**: the script runs as a child process with a bounded
//!      timeout. Exit 0 = success, anything else = failure. The runner
//!      captures stderr (tail) + duration so callers can include the
//!      detail in a wider operation log.
//!   3. **Auditability**: every hook attempt — success, failure, AND
//!      refusal due to path-safety or missing script — emits a
//!      [`LogKind::Component`] record to the central log so
//!      `anolisa logs` shows the hook in context with the operation that
//!      triggered it. Hooks are best-effort by default: a failed hook
//!      surfaces as a warning on the parent operation rather than aborting
//!      the whole verb. Callers that need a strict gate (rare today) can
//!      escalate by checking `outcome.success` explicitly.
//!
//! The runner is intentionally synchronous and side-effect-free at the
//! state-file level: it does NOT mutate `installed.toml`. Callers that
//! want the hook outcome reflected in state (e.g. record `last_run_at`
//! per phase) take care of it themselves under the install lock.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::adapter::{AdapterError, expand_layout_placeholders};
use crate::central_log::{CentralLog, LogKind, LogRecord, Severity};
use crate::manifest::ManifestHookSpec;
use crate::path_safety::{PathBoundaryError, validate_owned_path};
use anolisa_platform::fs_layout::FsLayout;

/// Phases the runner understands. Authored as a small finite set on
/// purpose — adding a phase is intentional API surface (manifests + spec
/// docs need to mention it) so we don't want a free-form string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPhase {
    /// Runs before component files are installed.
    PreInstall,
    /// Runs after component files are installed.
    PostInstall,
    /// Runs before a component is installed or reactivated.
    PreEnable,
    /// Runs after a component is installed or reactivated.
    PostEnable,
    /// Runs before a logical disable changes state.
    PreDisable,
    /// Runs after a logical disable changes state.
    PostDisable,
    /// Runs before uninstall removes owned files.
    PreUninstall,
    /// Runs after uninstall removes owned files.
    PostUninstall,
    /// Runs before a service/component restart.
    PreRestart,
    /// Runs after a service/component restart.
    PostRestart,
}

impl HookPhase {
    /// Stable wire-format name. Used as the `command` discriminator on
    /// the central log record so `anolisa logs --command=hook:pre_enable`
    /// (future filter) can target a single phase.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PreInstall => "pre_install",
            Self::PostInstall => "post_install",
            Self::PreEnable => "pre_enable",
            Self::PostEnable => "post_enable",
            Self::PreDisable => "pre_disable",
            Self::PostDisable => "post_disable",
            Self::PreUninstall => "pre_uninstall",
            Self::PostUninstall => "post_uninstall",
            Self::PreRestart => "pre_restart",
            Self::PostRestart => "post_restart",
        }
    }
}

/// What the caller wants the runner to execute. `script` is the absolute
/// path to a shell script (or executable). `timeout_secs` defaults to 30
/// — most lifecycle hooks are smoke-test scripts that finish in well
/// under a second; a runaway hook should not hang the whole CLI verb.
#[derive(Debug, Clone)]
pub struct HookSpec {
    /// Component that owns the hook script.
    pub component: String,
    /// Lifecycle phase that selected this script.
    pub phase: HookPhase,
    /// Absolute script path after discovery.
    pub script: PathBuf,
    /// Maximum wall-clock time allowed for the child process.
    pub timeout_secs: u32,
    /// When `false`, a failure is recorded and surfaced as a warning but
    /// the caller continues. When `true`, the caller is expected to
    /// short-circuit on `outcome.success == false`. The runner itself
    /// never aborts the parent verb — failure semantics live with the
    /// caller.
    pub strict: bool,
}

impl HookSpec {
    /// Build a hook spec with the alpha defaults: 30-second timeout and
    /// non-strict failure handling.
    pub fn new(component: impl Into<String>, phase: HookPhase, script: PathBuf) -> Self {
        Self {
            component: component.into(),
            phase,
            script,
            timeout_secs: 30,
            strict: false,
        }
    }
}

/// Categorical reason a hook didn't produce a useful exit code. Distinct
/// from `success: false` so callers can branch on "the hook ran and
/// failed" vs "the hook never even ran".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookSkipReason {
    /// Path-safety rejected the script.
    PathRejected(String),
    /// Script does not exist on disk.
    Missing,
    /// Script was not executable (chmod / OS error spawn).
    NotExecutable(String),
    /// Hook ran but exceeded its timeout. The child was killed.
    Timeout,
}

/// Result of a single hook run. Always populated even on skip — callers
/// rely on this to build per-phase warnings for the parent verb.
#[derive(Debug, Clone)]
pub struct HookOutcome {
    /// Component whose hook was attempted.
    pub component: String,
    /// Lifecycle phase that selected the hook.
    pub phase: HookPhase,
    /// Script path that was validated and possibly executed.
    pub script: PathBuf,
    /// `true` only when the hook ran and exited zero.
    pub success: bool,
    /// `Some` when the hook spawned (regardless of exit). `None` on every
    /// skip path including timeout (kill leaves no exit code on Unix).
    pub exit_code: Option<i32>,
    /// Wall-clock duration including spawn. `Duration::ZERO` on path-safety
    /// rejection (we never spawned).
    pub duration: Duration,
    /// Up to ~4KB of stderr captured for diagnostics. Never used to make
    /// control-flow decisions — only surfaced in the central log.
    pub stderr_tail: String,
    /// Populated when the hook didn't yield a useful exit code. Mutually
    /// exclusive with `success = true`.
    pub skip: Option<HookSkipReason>,
}

impl HookOutcome {
    /// One-line summary suitable for the `message` field of a central
    /// log record.
    pub fn summary(&self) -> String {
        match &self.skip {
            Some(HookSkipReason::PathRejected(reason)) => {
                format!(
                    "hook {} for {} skipped: path rejected ({reason})",
                    self.phase.as_str(),
                    self.component,
                )
            }
            Some(HookSkipReason::Missing) => {
                format!(
                    "hook {} for {} skipped: script not present",
                    self.phase.as_str(),
                    self.component,
                )
            }
            Some(HookSkipReason::NotExecutable(err)) => {
                format!(
                    "hook {} for {} failed to spawn: {err}",
                    self.phase.as_str(),
                    self.component,
                )
            }
            Some(HookSkipReason::Timeout) => {
                format!(
                    "hook {} for {} killed after {}s timeout",
                    self.phase.as_str(),
                    self.component,
                    self.duration.as_secs(),
                )
            }
            None if self.success => format!(
                "hook {} for {} succeeded in {}ms",
                self.phase.as_str(),
                self.component,
                self.duration.as_millis(),
            ),
            None => format!(
                "hook {} for {} exited {} after {}ms",
                self.phase.as_str(),
                self.component,
                self.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                self.duration.as_millis(),
            ),
        }
    }
}

/// Run `spec` against `layout`. Path-safety is mandatory; callers cannot
/// disable it (no `--allow-external` knob). When `log` is provided, every
/// hook attempt — success, failure, and skip — emits a single
/// component-scoped log record so `anolisa logs` can surface it.
///
/// `operation_id` is the parent verb's id (`op-<utc>-<n>`) so the user
/// can correlate "enable agent-observability ran these hooks". `actor`
/// and `install_mode` mirror the parent record's fields.
pub fn run_hook(
    spec: &HookSpec,
    layout: &FsLayout,
    log: Option<&CentralLog>,
    operation_id: &str,
    actor: &str,
    install_mode: &str,
) -> HookOutcome {
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let outcome = execute(spec, layout);
    if let Some(log) = log {
        let record = build_log_record(&outcome, operation_id, actor, install_mode, &started_at);
        // Best-effort log: a failed log append must not mask the hook
        // result the caller is waiting on. The central log itself logs
        // its own io errors at higher tiers.
        let _ = log.append(&record);
    }
    outcome
}

fn execute(spec: &HookSpec, layout: &FsLayout) -> HookOutcome {
    // Path-safety first — guard runs before any filesystem inspection.
    if let Err(err) = validate_owned_path(layout, &spec.script) {
        return HookOutcome {
            component: spec.component.clone(),
            phase: spec.phase,
            script: spec.script.clone(),
            success: false,
            exit_code: None,
            duration: Duration::ZERO,
            stderr_tail: String::new(),
            skip: Some(HookSkipReason::PathRejected(reason_for(&err))),
        };
    }
    if !spec.script.exists() {
        return HookOutcome {
            component: spec.component.clone(),
            phase: spec.phase,
            script: spec.script.clone(),
            success: false,
            exit_code: None,
            duration: Duration::ZERO,
            stderr_tail: String::new(),
            skip: Some(HookSkipReason::Missing),
        };
    }

    let started = Instant::now();
    let timeout = Duration::from_secs(u64::from(spec.timeout_secs.max(1)));
    // spawn_retry_etxtbsy: hook scripts are files ANOLISA itself wrote;
    // a concurrent fork elsewhere can hold the write descriptor for a
    // moment and fail exec with ETXTBSY.
    let mut child = match crate::process::spawn_retry_etxtbsy(
        Command::new(&spec.script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    ) {
        Ok(c) => c,
        Err(err) => {
            return HookOutcome {
                component: spec.component.clone(),
                phase: spec.phase,
                script: spec.script.clone(),
                success: false,
                exit_code: None,
                duration: started.elapsed(),
                stderr_tail: String::new(),
                skip: Some(HookSkipReason::NotExecutable(err.to_string())),
            };
        }
    };

    // Lightweight polling loop avoids pulling in a full async runtime
    // for what amounts to "wait <30s for one short script". 25ms gives
    // sub-second responsiveness for fast hooks without burning CPU.
    let poll = Duration::from_millis(25);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return HookOutcome {
                        component: spec.component.clone(),
                        phase: spec.phase,
                        script: spec.script.clone(),
                        success: false,
                        exit_code: None,
                        duration: started.elapsed(),
                        stderr_tail: String::new(),
                        skip: Some(HookSkipReason::Timeout),
                    };
                }
                std::thread::sleep(poll);
            }
            Err(err) => {
                return HookOutcome {
                    component: spec.component.clone(),
                    phase: spec.phase,
                    script: spec.script.clone(),
                    success: false,
                    exit_code: None,
                    duration: started.elapsed(),
                    stderr_tail: String::new(),
                    skip: Some(HookSkipReason::NotExecutable(err.to_string())),
                };
            }
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(err) => {
            return HookOutcome {
                component: spec.component.clone(),
                phase: spec.phase,
                script: spec.script.clone(),
                success: false,
                exit_code: None,
                duration: started.elapsed(),
                stderr_tail: String::new(),
                skip: Some(HookSkipReason::NotExecutable(err.to_string())),
            };
        }
    };

    let stderr_tail = tail_lossy(&output.stderr, 4096);
    let exit_code = output.status.code();
    let success = output.status.success();
    HookOutcome {
        component: spec.component.clone(),
        phase: spec.phase,
        script: spec.script.clone(),
        success,
        exit_code,
        duration: started.elapsed(),
        stderr_tail,
        skip: None,
    }
}

fn reason_for(err: &PathBoundaryError) -> String {
    match err {
        PathBoundaryError::External { path } => {
            format!("'{}' is not under an ANOLISA-owned root", path.display())
        }
        PathBoundaryError::Traversal { path } => {
            format!("'{}' contains '.' or '..'", path.display())
        }
    }
}

fn tail_lossy(bytes: &[u8], max: usize) -> String {
    let start = bytes.len().saturating_sub(max);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn build_log_record(
    outcome: &HookOutcome,
    operation_id: &str,
    actor: &str,
    install_mode: &str,
    started_at: &str,
) -> LogRecord {
    let severity = if outcome.success {
        Severity::Info
    } else if matches!(outcome.skip, Some(HookSkipReason::Missing)) {
        // Missing optional hook is information, not a warning — most
        // components don't ship every phase.
        Severity::Info
    } else {
        Severity::Warn
    };
    let finished_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    LogRecord {
        kind: LogKind::Component,
        operation_id: Some(operation_id.to_string()),
        command: format!("hook:{}", outcome.phase.as_str()),
        source: "anolisa-core".to_string(),
        component: Some(outcome.component.clone()),
        severity,
        message: outcome.summary(),
        actor: actor.to_string(),
        install_mode: Some(install_mode.to_string()),
        started_at: started_at.to_string(),
        finished_at: Some(finished_at),
        status: None,
        objects: vec![outcome.component.clone()],
        backup_ids: Vec::new(),
        warnings: Vec::new(),
        details: serde_json::json!({
            "phase": outcome.phase.as_str(),
            "script": outcome.script.display().to_string(),
            "exit_code": outcome.exit_code,
            "duration_ms": outcome.duration.as_millis() as u64,
            "stderr_tail": outcome.stderr_tail,
            "skip": outcome.skip.as_ref().map(skip_label),
        }),
    }
}

fn skip_label(skip: &HookSkipReason) -> &'static str {
    match skip {
        HookSkipReason::PathRejected(_) => "path_rejected",
        HookSkipReason::Missing => "missing",
        HookSkipReason::NotExecutable(_) => "not_executable",
        HookSkipReason::Timeout => "timeout",
    }
}

/// Best-effort: run a sequence of hooks, accumulating warnings for the
/// non-strict failures so a caller can attach them to the parent verb's
/// outcome. Stops at the first strict failure (returns the partial
/// outcome list so the caller can still log what ran).
pub fn run_hooks(
    specs: &[HookSpec],
    layout: &FsLayout,
    log: Option<&CentralLog>,
    operation_id: &str,
    actor: &str,
    install_mode: &str,
) -> HookRunResult {
    let mut outcomes = Vec::with_capacity(specs.len());
    let mut warnings: Vec<String> = Vec::new();
    let mut hard_failure: Option<HookOutcome> = None;
    for spec in specs {
        let outcome = run_hook(spec, layout, log, operation_id, actor, install_mode);
        if !outcome.success {
            // Missing hook is a no-op surface: most phases are unset and
            // we don't want to flood the parent verb with "no script for
            // post_enable".
            if !matches!(outcome.skip, Some(HookSkipReason::Missing)) {
                warnings.push(outcome.summary());
            }
            if spec.strict {
                hard_failure = Some(outcome.clone());
                outcomes.push(outcome);
                break;
            }
        }
        outcomes.push(outcome);
    }
    HookRunResult {
        outcomes,
        warnings,
        hard_failure,
    }
}

/// Aggregated result for a phase batch.
#[derive(Debug, Clone)]
pub struct HookRunResult {
    /// Outcomes collected before the batch completed or stopped at a
    /// strict failure.
    pub outcomes: Vec<HookOutcome>,
    /// Non-missing hook failures rendered by parent operations.
    pub warnings: Vec<String>,
    /// Set when a `strict = true` hook failed and the loop stopped.
    pub hard_failure: Option<HookOutcome>,
}

/// Resolve a component's contract-declared hooks for one `phase` into
/// executor inputs.
///
/// Hooks are **contract-driven**: a component declares them in
/// `[[component.hooks]]` (parsed into [`ManifestHookSpec`]), and the
/// installer persists that contract verbatim so the uninstall side can read
/// the real `script`/`strict`/`timeout` back. This is what lets a component
/// like ws-ckpt mark its `pre-uninstall` recover `strict = false` (a failed
/// recover warns instead of rolling back the uninstall) — a guarantee the
/// old `<phase>.sh` filename convention could not express.
///
/// `script` is an unexpanded layout-template path
/// (e.g. `{datadir}/hooks/ws-ckpt/pre-uninstall.sh`); it is expanded with
/// the same [`expand_layout_placeholders`] machinery used for adapters,
/// with `{component}` bound so per-component paths resolve. Path-safety is
/// enforced downstream by [`run_hook`] regardless, so an expanded path that
/// escapes ANOLISA's owned roots is still refused before spawn.
///
/// # Errors
///
/// Returns [`AdapterError::UnknownPlaceholder`] when a `script` template
/// references a placeholder that is neither a layout field nor `{component}`.
pub fn resolve_manifest_hooks(
    manifest_hooks: &[ManifestHookSpec],
    layout: &FsLayout,
    component: &str,
    phase: HookPhase,
) -> Result<Vec<HookSpec>, AdapterError> {
    let mut specs = Vec::new();
    for h in manifest_hooks.iter().filter(|h| h.phase == phase) {
        let script = expand_layout_placeholders(&h.script, layout, &[("component", component)])?;
        specs.push(HookSpec {
            component: component.to_string(),
            phase,
            script,
            timeout_secs: h.timeout_secs,
            strict: h.strict,
        });
    }
    Ok(specs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_platform::fs_layout::FsLayout;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tempfile::tempdir;

    fn layout_with(prefix: &Path) -> FsLayout {
        let layout = FsLayout::system(Some(prefix.to_path_buf()));
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        fs::create_dir_all(&layout.datadir).expect("mkdir datadir");
        fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        layout
    }

    fn write_script(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir parent");
        }
        let mut file = fs::File::create(path).expect("create script");
        file.write_all(body.as_bytes()).expect("write script");
        file.sync_all().expect("sync script");
        drop(file);
        let mut perm = fs::metadata(path).expect("stat").permissions();
        perm.set_mode(0o755);
        fs::set_permissions(path, perm).expect("chmod");
    }

    #[test]
    fn phase_as_str_round_trips() {
        // Stable wire labels — the central log filter relies on them.
        assert_eq!(HookPhase::PreEnable.as_str(), "pre_enable");
        assert_eq!(HookPhase::PostEnable.as_str(), "post_enable");
        assert_eq!(HookPhase::PreUninstall.as_str(), "pre_uninstall");
    }

    #[test]
    fn hook_under_owned_root_runs_and_exits_zero() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let script = layout.datadir.join("hooks/foo/post_enable.sh");
        write_script(&script, "#!/bin/sh\nexit 0\n");

        let spec = HookSpec::new("foo", HookPhase::PostEnable, script.clone());
        let outcome = run_hook(&spec, &layout, None, "op-test-1", "tester", "system");
        assert!(outcome.success, "hook should succeed: {outcome:?}");
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.skip.is_none());
    }

    #[test]
    fn hook_outside_owned_root_is_refused_without_running() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        // Script that DOES exist and is executable but lives outside
        // every owned root. The runner must refuse on path-safety.
        let outside = dir.path().join("escape.sh");
        write_script(&outside, "#!/bin/sh\nexit 0\n");

        let spec = HookSpec::new("foo", HookPhase::PreEnable, outside.clone());
        let outcome = run_hook(&spec, &layout, None, "op-test-2", "tester", "system");
        assert!(!outcome.success, "must refuse external path");
        assert!(matches!(
            outcome.skip,
            Some(HookSkipReason::PathRejected(_))
        ));
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.duration, Duration::ZERO, "never spawned");
    }

    #[test]
    fn missing_script_is_a_skip_not_a_failure_signal() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let script = layout.datadir.join("hooks/foo/never_existed.sh");
        // Note: parent dir created so path-safety doesn't fail before
        // the existence check runs.
        fs::create_dir_all(script.parent().unwrap()).expect("mkdir hooks");

        let spec = HookSpec::new("foo", HookPhase::PostEnable, script);
        let outcome = run_hook(&spec, &layout, None, "op-test-3", "tester", "system");
        assert_eq!(outcome.skip, Some(HookSkipReason::Missing));
        assert!(!outcome.success);
        assert!(outcome.summary().contains("skipped"));
    }

    #[test]
    fn nonzero_exit_yields_unsuccessful_outcome_with_exit_code() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let script = layout.datadir.join("hooks/foo/pre_disable.sh");
        write_script(&script, "#!/bin/sh\necho oops 1>&2\nexit 7\n");

        let spec = HookSpec::new("foo", HookPhase::PreDisable, script.clone());
        let outcome = run_hook(&spec, &layout, None, "op-test-4", "tester", "system");
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, Some(7));
        assert!(outcome.skip.is_none());
        assert!(outcome.stderr_tail.contains("oops"));
    }

    #[test]
    fn central_log_records_one_line_per_hook_attempt() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let script = layout.datadir.join("hooks/foo/post_install.sh");
        write_script(&script, "#!/bin/sh\nexit 0\n");

        let log_path = layout.log_dir.join("anolisa.log");
        fs::create_dir_all(log_path.parent().unwrap()).expect("mkdir log");
        let log = CentralLog::open(log_path.clone());

        let spec = HookSpec::new("foo", HookPhase::PostInstall, script);
        run_hook(&spec, &layout, Some(&log), "op-test-5", "tester", "system");

        let raw = fs::read_to_string(&log_path).expect("read log");
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one log line, got: {raw}");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("parse log");
        assert_eq!(parsed["kind"], "component");
        assert_eq!(parsed["command"], "hook:post_install");
        assert_eq!(parsed["component"], "foo");
        assert_eq!(parsed["operation_id"], "op-test-5");
        assert_eq!(parsed["details"]["phase"], "post_install");
        assert_eq!(parsed["details"]["exit_code"], 0);
    }

    #[test]
    fn run_hooks_aggregates_warnings_and_continues_on_nonstrict_failure() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let ok = layout.datadir.join("hooks/foo/pre_enable.sh");
        let bad = layout.datadir.join("hooks/foo/post_enable.sh");
        write_script(&ok, "#!/bin/sh\nexit 0\n");
        write_script(&bad, "#!/bin/sh\nexit 1\n");

        let specs = vec![
            HookSpec::new("foo", HookPhase::PreEnable, ok.clone()),
            HookSpec::new("foo", HookPhase::PostEnable, bad.clone()),
        ];
        let result = run_hooks(&specs, &layout, None, "op-test-6", "tester", "system");
        assert_eq!(result.outcomes.len(), 2, "both ran");
        assert!(result.outcomes[0].success);
        assert!(!result.outcomes[1].success);
        assert!(result.hard_failure.is_none(), "non-strict, no hard fail");
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("post_enable"));
    }

    #[test]
    fn run_hooks_stops_at_first_strict_failure() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let bad = layout.datadir.join("hooks/foo/pre_enable.sh");
        let after = layout.datadir.join("hooks/foo/post_enable.sh");
        write_script(&bad, "#!/bin/sh\nexit 5\n");
        write_script(&after, "#!/bin/sh\nexit 0\n");

        let mut strict = HookSpec::new("foo", HookPhase::PreEnable, bad.clone());
        strict.strict = true;
        let specs = vec![
            strict,
            HookSpec::new("foo", HookPhase::PostEnable, after.clone()),
        ];
        let result = run_hooks(&specs, &layout, None, "op-test-7", "tester", "system");
        assert_eq!(result.outcomes.len(), 1, "stopped at first strict fail");
        assert!(result.hard_failure.is_some());
        assert_eq!(result.hard_failure.unwrap().exit_code, Some(5));
    }

    #[test]
    fn timeout_kills_hook_and_records_skip() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let script = layout.datadir.join("hooks/foo/pre_enable.sh");
        write_script(&script, "#!/bin/sh\nsleep 5\n");

        let mut spec = HookSpec::new("foo", HookPhase::PreEnable, script.clone());
        spec.timeout_secs = 1;
        let outcome = run_hook(&spec, &layout, None, "op-test-8", "tester", "system");
        assert!(!outcome.success);
        assert_eq!(outcome.skip, Some(HookSkipReason::Timeout));
        assert!(outcome.duration >= Duration::from_secs(1));
        assert!(
            outcome.duration < Duration::from_secs(5),
            "should not wait full 5s"
        );
    }

    #[test]
    fn resolve_manifest_hooks_expands_path_and_carries_contract_fields() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let manifest_hooks = vec![ManifestHookSpec {
            phase: HookPhase::PreUninstall,
            script: "{datadir}/hooks/ws-ckpt/pre-uninstall.sh".to_string(),
            timeout_secs: 45,
            strict: false,
        }];
        let specs =
            resolve_manifest_hooks(&manifest_hooks, &layout, "ws-ckpt", HookPhase::PreUninstall)
                .expect("resolve ok");
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(
            spec.script,
            layout.datadir.join("hooks/ws-ckpt/pre-uninstall.sh"),
        );
        assert_eq!(spec.component, "ws-ckpt");
        assert_eq!(spec.phase, HookPhase::PreUninstall);
        assert_eq!(spec.timeout_secs, 45);
        // ws-ckpt's recover MUST stay non-strict so a failed recover warns
        // instead of rolling back the uninstall.
        assert!(!spec.strict);
    }

    #[test]
    fn resolve_manifest_hooks_filters_by_phase() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let manifest_hooks = vec![
            ManifestHookSpec {
                phase: HookPhase::PreUninstall,
                script: "{datadir}/hooks/c/pre-uninstall.sh".to_string(),
                timeout_secs: 30,
                strict: true,
            },
            ManifestHookSpec {
                phase: HookPhase::PostUninstall,
                script: "{datadir}/hooks/c/post-uninstall.sh".to_string(),
                timeout_secs: 30,
                strict: false,
            },
        ];
        let pre = resolve_manifest_hooks(&manifest_hooks, &layout, "c", HookPhase::PreUninstall)
            .expect("resolve ok");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].phase, HookPhase::PreUninstall);
        assert!(pre[0].strict);
    }

    #[test]
    fn resolve_manifest_hooks_rejects_unknown_placeholder() {
        let dir = tempdir().expect("tmpdir");
        let layout = layout_with(dir.path());
        let manifest_hooks = vec![ManifestHookSpec {
            phase: HookPhase::PreUninstall,
            script: "{nope}/pre-uninstall.sh".to_string(),
            timeout_secs: 30,
            strict: false,
        }];
        let err = resolve_manifest_hooks(&manifest_hooks, &layout, "c", HookPhase::PreUninstall)
            .expect_err("unknown placeholder must error");
        assert!(matches!(err, AdapterError::UnknownPlaceholder { .. }));
    }
}
