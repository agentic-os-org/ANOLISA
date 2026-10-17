use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;
use sha2::{Digest, Sha256};
use wait_timeout::ChildExt;

use super::raw_cli::{configure_raw_cli_command, raw_cli_shared_run_guard, RawCliRunGuard};

const READY_MARKER: &str = "cosh-osc$";
const NATIVE_PROMPT_MARKER: &str = "owner@host:~$";
/// Rendered label of `ApprovalPanelAction::Approve` on a high-risk card. The
/// descriptor name and the on-screen text differ, so the oracle matches what
/// the user actually sees.
const APPROVE_LABEL: &str = "Allow once";
/// Start of the command preview as the card must show it. `request.preview`
/// already carries the `$ ` marker from `tools/display.rs`, so this is the whole
/// marker budget the card has.
const PREVIEW_MARKER: &str = "$ printf";
const TOOL_CALL_ID: &str = "slice-tool-1";
const EFFECT_LINE: &str = "slice_effect";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const STEP_TIMEOUT: Duration = Duration::from_secs(45);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const QUIET: Duration = Duration::from_millis(50);
const EVIDENCE_ROOT: &str = "/tmp/cosh-ui-approval-slice-evidence";
/// Basename of the standalone renderer process (Spec v2 §6.3).
const WORKER_EXE: &str = "cosh-ui-worker";

/// Debug-only slice policy (Spec v2 §4). Test candidate values, never release
/// defaults.
pub(crate) const SLICE_TEST_POLICY: [(&str, &str); 11] = [
    ("COSH_UI_TEST_MAX_PAYLOAD_BYTES", "1048576"),
    ("COSH_UI_TEST_MAX_DRAW_OPS", "4096"),
    ("COSH_UI_TEST_MAX_TEXT_BYTES", "16384"),
    ("COSH_UI_TEST_MAX_SURFACE_ROWS", "24"),
    ("COSH_UI_TEST_HANDSHAKE_TIMEOUT_MS", "5000"),
    ("COSH_UI_TEST_RENDER_TIMEOUT_MS", "5000"),
    ("COSH_UI_TEST_HEARTBEAT_INTERVAL_MS", "100"),
    // The relay loop's own maintenance interval is one second, so a one-second
    // heartbeat timeout would declare a healthy worker hung between two pumps.
    ("COSH_UI_TEST_HEARTBEAT_TIMEOUT_MS", "3000"),
    ("COSH_UI_TEST_BACKPRESSURE_TIMEOUT_MS", "5000"),
    ("COSH_UI_TEST_SHUTDOWN_TIMEOUT_MS", "2000"),
    ("COSH_UI_TEST_FAILURE_LIMIT", "3"),
];

/// Why the AB-00 oracle refused to certify a run. A negative control must land
/// on the exact variant it was written for, so a wrong rejection still fails.
/// The payload fields are surfaced through `Debug` in the failing assertion,
/// which is not a read for dead-code analysis.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum OracleError {
    CoreNotElf,
    ApprovalCardMissing,
    ApproveNotFocused,
    HostResultMissing,
    ToolsMissing,
    RequestCount { actual: usize },
    EffectCount { actual: usize },
    AuditCount { event: &'static str, actual: usize },
    WorkerDescendants { count: usize },
    ScreenMissing { needle: &'static str },
    BashFactChanged { detail: String },
    ExitStatus { code: Option<i32> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServerScript {
    Baseline,
    /// Records request 2 with its `tool` messages stripped, simulating a
    /// provider that never received the host-executed result. The oracle must
    /// catch this on an unmutated product, otherwise the host-result assertion
    /// is vacuous.
    OmitHostResult,
    DelayedEffect,
    TimeoutEffect,
}

#[derive(Clone, Debug)]
pub(crate) enum CoreChoice {
    RealElf,
    ScriptForMutation(PathBuf),
}

#[derive(Clone, Debug)]
pub(crate) struct SliceLaunch {
    pub(crate) activation: bool,
    pub(crate) core: CoreChoice,
    pub(crate) approval_mode: &'static str,
    pub(crate) server_script: ServerScript,
    pub(crate) worker_profile: Option<PathBuf>,
    /// Replaces the `cosh-ui-worker` candidate. Used to inject a worker that
    /// speaks malformed protocol, which the design requires the harness to do by
    /// substituting the first-generation candidate rather than by adding a fault
    /// hook to the runtime.
    pub(crate) worker_binary: Option<PathBuf>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) log_directory_blocked: bool,
    pub(crate) output_fault_library: Option<PathBuf>,
    pub(crate) output_fault_before_receipt: bool,
    pub(crate) handoff_timeout_secs: Option<u64>,
}

impl SliceLaunch {
    pub(crate) fn inline() -> Self {
        Self {
            activation: false,
            core: CoreChoice::RealElf,
            approval_mode: "auto",
            server_script: ServerScript::Baseline,
            worker_profile: None,
            worker_binary: None,
            cols: 40,
            rows: 24,
            log_directory_blocked: false,
            output_fault_library: None,
            output_fault_before_receipt: false,
            handoff_timeout_secs: None,
        }
    }

    /// Worker launch used by the eligibility and fault cases (Task 2 onward).
    #[allow(dead_code)]
    pub(crate) fn activated() -> Self {
        let worker_profile = std::env::var_os("COSH_ACCEPT_RELEASE_PROFILE").map(PathBuf::from);
        Self {
            activation: worker_profile.is_none(),
            worker_profile,
            ..Self::inline()
        }
    }
}

/// Sync points recorded while driving the slice; read back by the resize and
/// re-anchor cases and by the Task 15 evidence manifest.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct TimelineEntry {
    pub(crate) label: &'static str,
    pub(crate) offset: usize,
    pub(crate) row: u16,
    pub(crate) col: u16,
}

/// Everything one slice run produced. `timeline` and `ledger` are consumed by
/// the Task 15 evidence manifest rather than by `check_inline_baseline`.
#[allow(dead_code)]
pub(crate) struct ApprovalSliceObservation {
    pub(crate) http_bodies: Vec<serde_json::Value>,
    pub(crate) effect_count: usize,
    pub(crate) audit: Vec<String>,
    pub(crate) transcript: String,
    pub(crate) timeline: Vec<TimelineEntry>,
    pub(crate) ledger: serde_json::Value,
    pub(crate) screen: String,
    /// Approval/tool audit records plus the tail of every host log, dumped only
    /// on failure so a routing drop can be attributed without a second run.
    pub(crate) diagnostics: String,
    core_sha256: Option<String>,
    worker_descendants: usize,
    bash_changed: Option<String>,
    exit_code: Option<i32>,
    failures: Vec<OracleError>,
}

impl ApprovalSliceObservation {
    /// Oracle for the deny path.
    ///
    /// Safety here means four facts, not the absence of a panic: the approved
    /// command never ran, the denial was still recorded as a resolution, no
    /// worker process survived the interaction, and the Bash facts the harness
    /// captured at startup are unchanged.
    /// How many audit records carry `event`.
    ///
    /// Exposed so the deny path can be compared between launch modes: whether a
    /// denial records one resolution or two is a product fact about the inline
    /// path, and the slice has to match it rather than match a guess.
    pub(crate) fn audit_event_count(&self, event: &str) -> usize {
        count_audit(&self.audit, event)
    }

    pub(crate) fn check_denied_slice(self) -> Result<(), OracleError> {
        if let Some(first) = self.failures.first().cloned() {
            eprintln!(
                "DENY effect_count={} worker_descendants={} screen=\n{}\ndiagnostics=\n{}",
                self.effect_count, self.worker_descendants, self.screen, self.diagnostics
            );
            return Err(first);
        }
        if self.effect_count != 0 {
            return Err(OracleError::EffectCount {
                actual: self.effect_count,
            });
        }
        let started = count_audit(&self.audit, "tool.execution.started");
        if started != 0 {
            return Err(OracleError::AuditCount {
                event: "tool.execution.started",
                actual: started,
            });
        }
        if self.worker_descendants != 0 {
            return Err(OracleError::WorkerDescendants {
                count: self.worker_descendants,
            });
        }
        if let Some(detail) = self.bash_changed {
            return Err(OracleError::BashFactChanged { detail });
        }
        if self.exit_code.is_none() {
            return Err(OracleError::ExitStatus { code: None });
        }
        Ok(())
    }

    pub(crate) fn check_inline_baseline(self) -> Result<(), OracleError> {
        if let Some(first) = self.failures.first().cloned() {
            eprintln!(
                "AB-00 http_requests={} first_has_tools={} effect_count={} audit_events={:?} ledger={}",
                self.http_bodies.len(),
                self.http_bodies.first().is_some_and(|body| body.get("tools").is_some()),
                self.effect_count,
                audit_event_types(&self.audit),
                self.ledger,
            );
            eprintln!(
                "AB-00 screen dump:\n{}\nAB-00 diagnostics:\n{}\nAB-00 transcript tail:\n{}",
                self.screen,
                self.diagnostics,
                &self.transcript[self.transcript.len().saturating_sub(4000)..]
            );
            return Err(first);
        }
        if self.core_sha256.is_none() {
            return Err(OracleError::CoreNotElf);
        }
        if self.http_bodies.len() != 3 {
            return Err(OracleError::RequestCount {
                actual: self.http_bodies.len(),
            });
        }
        if self.http_bodies[0].get("tools").is_none() {
            return Err(OracleError::ToolsMissing);
        }
        if !has_tool_result(&self.http_bodies[1]) {
            return Err(OracleError::HostResultMissing);
        }
        if self.effect_count != 1 {
            return Err(OracleError::EffectCount {
                actual: self.effect_count,
            });
        }
        for event in ["approval.resolved", "tool.execution.started"] {
            let needle = format!("\"event_type\":\"{event}\"");
            let actual = self
                .audit
                .iter()
                .filter(|line| line.contains(&needle))
                .count();
            if actual != 1 {
                return Err(OracleError::AuditCount { event, actual });
            }
        }
        if self.worker_descendants != 0 {
            return Err(OracleError::WorkerDescendants {
                count: self.worker_descendants,
            });
        }
        for needle in ["slice-ordinary-ok", "slice second response"] {
            if !self.screen.contains(needle) {
                return Err(OracleError::ScreenMissing { needle });
            }
        }
        if let Some(detail) = self.bash_changed {
            return Err(OracleError::BashFactChanged { detail });
        }
        if self.exit_code != Some(0) {
            return Err(OracleError::ExitStatus {
                code: self.exit_code,
            });
        }
        Ok(())
    }
}

fn has_tool_result(body: &serde_json::Value) -> bool {
    body.get("messages")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message
                    .get("tool_call_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(TOOL_CALL_ID)
            })
        })
}

pub(crate) struct RealCoreApprovalHarness {
    terminal: SliceTerminal,
    server: DeterministicOpenAiServer,
    ledger: ProcessLedger,
    markers: HostLogMarkers,
    root: tempfile::TempDir,
    effect_file: PathBuf,
    core_sha256: Option<String>,
    bash: Option<BashFacts>,
    fatal: Option<OracleError>,
    failures: Vec<OracleError>,
    _gate: RawCliRunGuard,
}

impl RealCoreApprovalHarness {
    pub(crate) fn spawn(launch: SliceLaunch) -> Self {
        let gate = raw_cli_shared_run_guard();
        let root = tempfile::Builder::new()
            .prefix("cosh-slice-")
            .tempdir()
            .expect("slice HOME");
        fs::create_dir_all(root.path().join(".copilot-shell")).expect("create config dir");
        if launch.log_directory_blocked {
            fs::write(root.path().join(".copilot-shell/logs"), b"not a directory")
                .expect("regular-file log directory fault");
        }
        if launch.output_fault_library.is_some() {
            fs::write(
                root.path().join(".bashrc"),
                "PS1='\\[\\e[32m\\]owner@host:\\w$\\[\\e[0m\\] '\n",
            )
            .expect("native prompt fixture");
        }
        let effect_file = root.path().join("effect.txt");
        let (server, port) =
            DeterministicOpenAiServer::start(launch.server_script, effect_file.clone());

        let (core_path, core_sha256, fatal) = match &launch.core {
            CoreChoice::RealElf => {
                let path = real_cosh_core_path();
                match assert_real_core(&path) {
                    Ok(sha) => (path, Some(sha), None),
                    Err(error) => (path, None, Some(error)),
                }
            }
            CoreChoice::ScriptForMutation(path) => match assert_real_core(path) {
                Ok(sha) => (path.clone(), Some(sha), None),
                Err(error) => (path.clone(), None, Some(error)),
            },
        };

        let label = slice_label();
        let mut ledger = ProcessLedger::new(&label, root.path());
        let terminal = SliceTerminal::spawn(&launch, &core_path, port, root.path());
        let shell_pid = terminal.child_pid();
        ledger.record("cosh-shell", shell_pid);
        let markers = HostLogMarkers::new(root.path(), shell_pid);

        let mut harness = Self {
            terminal,
            server,
            ledger,
            markers,
            root,
            effect_file,
            core_sha256,
            bash: None,
            fatal,
            failures: Vec::new(),
            _gate: gate,
        };
        if harness.fatal.is_some() {
            return harness;
        }
        let ready_marker = if launch.output_fault_library.is_some() {
            NATIVE_PROMPT_MARKER
        } else {
            READY_MARKER
        };
        if !harness
            .terminal
            .wait_for("startup-ready", ready_marker, STARTUP_TIMEOUT)
        {
            harness.fail(OracleError::ApprovalCardMissing);
            return harness;
        }
        if launch.log_directory_blocked {
            assert!(
                harness.stderr_logging_visible(),
                "file-sink failure must really write tracing to this PTY"
            );
            let log_dir = harness.root.path().join(".copilot-shell/logs");
            fs::remove_file(&log_dir).expect("remove owned logger fault before Core starts");
            fs::create_dir(&log_dir)
                .expect("Core gets a writable directory; shell subscriber remains stderr");
        }
        harness.capture_bash_facts();
        harness
    }

    fn fail(&mut self, error: OracleError) {
        if !self
            .failures
            .iter()
            .any(|existing| std::mem::discriminant(existing) == std::mem::discriminant(&error))
        {
            eprintln!("AB-00 harness failure: {error:?}");
            self.failures.push(error);
        }
    }

    fn capture_bash_facts(&mut self) {
        let shell_pid = self.terminal.child_pid();
        let Some(bash_pid) = descendant_with_name(shell_pid, "bash")
            .or_else(|| direct_children(shell_pid).into_iter().next())
        else {
            self.fail(OracleError::BashFactChanged {
                detail: "no bash descendant after startup".to_string(),
            });
            return;
        };
        self.terminal.send_line("sleep 600 & echo slice-bg:$!");
        let background_job = self
            .terminal
            .capture_pattern("background-job", "slice-bg:", STEP_TIMEOUT)
            .unwrap_or_default();
        self.bash = Some(BashFacts {
            pid: bash_pid,
            start_ticks: start_ticks(bash_pid),
            cwd: read_link(&format!("/proc/{bash_pid}/cwd")),
            background_job,
        });
    }

    pub(crate) fn assert_native_prompt_refault_was_injected(&self) {
        let hit = fs::read_to_string(self.root.path().join("output-hit"))
            .expect("physical fault evidence");
        assert!(hit.contains("EIO during native prompt recovery"));
    }

    pub(crate) fn persistent_output_fault_and_recovery(
        &mut self,
        timed_out: bool,
    ) -> ApprovalSliceObservation {
        self.ledger.scan_tree(self.terminal.child_pid());
        self.terminal.send(b"\r");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.root.path().join("output-hit").exists() && Instant::now() < deadline {
            self.terminal.read_output();
        }
        assert!(
            self.root.path().join("output-hit").exists(),
            "physical output fault must trigger after business approval"
        );
        assert_eq!(
            count_audit(&read_audit_lines(self.root.path()), "approval.resolved"),
            1,
            "fault is postcommit"
        );
        let hit = fs::read_to_string(self.root.path().join("output-hit"))
            .expect("actual injected failure");
        if hit.contains("after CUP") {
            assert!(
                !self.terminal.raw_transcript().contains("Approved"),
                "withdraw fault must precede the Approved receipt"
            );
            let stage_deadline = Instant::now() + Duration::from_secs(5);
            while !self.root.path().join("stage-hit").exists() && Instant::now() < stage_deadline {
                self.terminal.read_output();
            }
            assert!(
                self.root.path().join("stage-hit").exists(),
                "actual handoff sidecar write must be paused before master write"
            );
            self.terminal
                .send_line("echo forbidden-before-master > forbidden-effect");
            let input_deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < input_deadline {
                self.terminal.read_output();
            }
            let leaked = self.root.path().join("forbidden-effect").exists();
            fs::write(
                self.root.path().join("stage-release"),
                b"resume actual write",
            )
            .unwrap();
            assert!(
                !leaked,
                "known physical failure must hold real new input throughout sidecar staging"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut next_held_input = Instant::now();
        while self.server.bodies().len() < 2 && Instant::now() < deadline {
            if timed_out && Instant::now() >= next_held_input {
                self.terminal
                    .send_line("echo forbidden-during-output-failure > forbidden-effect");
                next_held_input = Instant::now() + Duration::from_millis(40);
            }
            self.terminal.read_output();
        }
        assert!(
            !self.root.path().join("forbidden-effect").exists(),
            "input throughout timeout must remain held"
        );
        assert_eq!(
            self.effect_lines(),
            usize::from(!timed_out),
            "old bounded timeout must interrupt, normal approved effect must finish"
        );
        assert_eq!(
            self.server.bodies().len(),
            2,
            "HostExecutedResult must reach real Core while output is broken"
        );
        let bodies = self.server.bodies();
        let host_results: Vec<_> = bodies[1]["messages"]
            .as_array()
            .expect("real Core messages")
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        assert_eq!(
            host_results.len(),
            1,
            "one real HostResult while EIO remains armed"
        );
        assert!(host_results[0]["content"]
            .as_str()
            .expect("tool result")
            .contains("ShellCommandCompleted"));
        if timed_out {
            assert!(
                host_results[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("timed_out"),
                "old timeout result is explicit: {}",
                host_results[0]
            );
        }
        eprintln!(
            "persistent-output: approval=1 effect={} HostResult=1 sameBash={:?} stdout=EIO",
            self.effect_lines(),
            self.bash.as_ref().map(|facts| facts.pid)
        );
        assert_eq!(
            self.bash_drift(),
            None,
            "same Bash/job/cwd must survive physical output failure"
        );
        assert!(self
            .terminal
            .child
            .try_wait()
            .expect("shell status")
            .is_none());
        self.terminal
            .send_line("echo forbidden-during-output-failure > forbidden-effect");
        let held_deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < held_deadline {
            self.terminal.read_output();
        }
        assert!(!self.root.path().join("forbidden-effect").exists());
        assert_eq!(
            count_audit(&read_audit_lines(self.root.path()), "approval.resolved"),
            1
        );
        fs::remove_file(self.root.path().join("output-arm")).expect("restore real output");
        assert!(
            self.terminal
                .wait_for("output-recovered", "slice first done", STEP_TIMEOUT),
            "recovered output must show the original run result"
        );
        if timed_out {
            assert!(
                self.terminal.wait_for(
                    "timeout-notice-restored",
                    "handoff timeout (2s).",
                    STEP_TIMEOUT
                ),
                "timeout notice must survive physical failure"
            );
        }
        assert!(self.terminal.wait_for(
            "native-prompt-recovered",
            NATIVE_PROMPT_MARKER,
            STEP_TIMEOUT
        ));
        let screen = self.terminal.parser.screen();
        let (row, col) = screen.cursor_position();
        let cursor_line = screen
            .rows(0, self.terminal.cols)
            .nth(usize::from(row))
            .unwrap_or_default();
        assert!(
            !screen.hide_cursor(),
            "native editable cursor must be visible before input"
        );
        assert_eq!(
            cursor_line.trim_end(),
            NATIVE_PROMPT_MARKER,
            "row={row} col={col} screen={:?} faults={:?} transcript={:?}",
            screen.contents(),
            fs::read_to_string(self.root.path().join("output-hit")),
            self.terminal.raw_transcript()
        );
        assert_eq!(usize::from(col), NATIVE_PROMPT_MARKER.len() + 1);
        assert_eq!(screen.cell(row, 0).unwrap().fgcolor(), vt100::Color::Idx(2));
        eprintln!("native-prompt-recovered: row={row} col={col} line={cursor_line:?}");
        eprintln!(
            "physical-output-faults: {}",
            fs::read_to_string(self.root.path().join("output-hit")).expect("fault evidence")
        );
        self.terminal.send_line("printf 'slice-ordinary-%s\n' ok");
        assert!(self
            .terminal
            .wait_for("ordinary", "slice-ordinary-ok", STEP_TIMEOUT));
        assert!(
            !self.root.path().join("forbidden-effect").exists(),
            "held old input must not leak after recovery"
        );
        self.terminal.send_line("?? slice second request");
        assert!(self
            .terminal
            .wait_for("second", "slice second response", STEP_TIMEOUT));
        let drift = self.bash_drift();
        let exit = self.shutdown();
        let observation = self.observe(drift).with_exit(exit);
        assert!(
            observation.exit_code.is_some(),
            "normal shell exit after recovery"
        );
        assert_eq!(
            observation.worker_descendants, 0,
            "no worker remains after cleanup"
        );
        observation
    }

    pub(crate) fn run_approve_execute_shell_second_request(&mut self) -> ApprovalSliceObservation {
        if self.fatal.is_none() && !self.await_visible_card() {
            return self.observe(None);
        }
        self.approve_execute_shell_second_request_from_visible_card()
    }

    fn await_visible_card(&mut self) -> bool {
        self.terminal.send_line("?? slice approval request");
        let needles = if self.root.path().join("output-arm").exists() {
            [APPROVE_LABEL, "Deny", "Details", "slice_e", "$ sleep"]
        } else {
            [
                APPROVE_LABEL,
                "Deny",
                "Details",
                EFFECT_LINE,
                PREVIEW_MARKER,
            ]
        };
        if !self
            .terminal
            .wait_for_all("approval-card", &needles, STEP_TIMEOUT)
        {
            self.fail(OracleError::ApprovalCardMissing);
            return false;
        }
        self.terminal.mark("card-visible");
        let screen = self.terminal.screen_text();
        assert!(
            !screen.contains("$ $"),
            "the card must carry one command marker, not two:\n{screen}"
        );
        if !approve_is_focused(&self.terminal.screen_text()) {
            self.fail(OracleError::ApproveNotFocused);
            return false;
        }
        true
    }

    /// Drives the request until the approval card is on screen, whichever
    /// process rendered it. Returns false if no card appears.
    pub(crate) fn request_approval_card(&mut self) -> bool {
        self.fatal.is_none() && self.await_visible_card()
    }

    pub(crate) fn seed_reflow_long_line(&mut self) -> bool {
        let padding = "abcdefgh终端输出".repeat(16);
        self.terminal.send_line(&format!(
            "printf '%s%s%s%s%s\\n' REFLOW_LEFT_ SENTINEL '{padding}' REFLOW_RIGHT_ SENTINEL; printf '%s%s\\n' REFLOW_READY_ SENTINEL"
        ));
        self.terminal.wait_for(
            "reflow-long-line-ready",
            "REFLOW_READY_SENTINEL",
            STEP_TIMEOUT,
        )
    }

    pub(crate) fn current_diagnostics(&self) -> String {
        build_diagnostics(&read_audit_lines(self.root.path()), self.root.path())
    }

    pub(crate) fn stderr_logging_visible(&self) -> bool {
        self.terminal
            .raw_transcript()
            .contains("cosh-shell starting")
    }

    pub(crate) fn visible_approval_cards(&mut self) -> usize {
        self.terminal.poll_screen().matches(APPROVE_LABEL).count()
    }

    /// Live `cosh-ui-worker` descendants of the shell, polled until at least
    /// one appears or the timeout passes. Every sighting is recorded in the
    /// ledger so cleanup can reap it.
    pub(crate) fn worker_descendants_within(&mut self, timeout: Duration) -> Vec<u32> {
        let shell_pid = self.terminal.child_pid();
        let deadline = Instant::now() + timeout;
        loop {
            self.ledger.scan_tree(shell_pid);
            let workers: Vec<u32> = descendants(shell_pid)
                .into_iter()
                .filter(|pid| exe_name(*pid) == WORKER_EXE)
                .collect();
            if !workers.is_empty() || Instant::now() >= deadline {
                if let Some(profile) = std::env::var_os("COSH_ACCEPT_RELEASE_PROFILE") {
                    for &pid in &workers {
                        self.ledger
                            .worker_identities
                            .push(production_worker_identity(
                                pid,
                                shell_pid,
                                &PathBuf::from(&profile),
                            ));
                    }
                }
                self.ledger.write();
                return workers;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Moves the card focus right and back left, returning the settled screen
    /// before and after.
    ///
    /// Approval actions are packed horizontally, so `apply_arrow` only maps
    /// Left/Right for an approval capture: Up/Down are deliberately inert and
    /// would prove nothing.
    ///
    /// Each direction waits for the repaint to actually land rather than sleeping
    /// a fixed time. Comparing the two screens is what separates an in-place
    /// repaint from a withdraw-and-reacquire: the latter scrolls the surface
    /// height off the top of the visible screen every time, so the history above
    /// the card cannot survive it.
    pub(crate) fn focus_change_round_trip(&mut self, timeout: Duration) -> (String, String) {
        let before = self.settled_screen(timeout);
        assert!(
            approve_is_focused(&before),
            "Approve must start focused: {before}"
        );
        self.terminal.send(b"\x1b[C");
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && approve_is_focused(&self.terminal.poll_screen()) {
            thread::sleep(Duration::from_millis(20));
        }
        self.terminal.mark("focus-right");
        assert!(
            !approve_is_focused(&self.terminal.poll_screen()),
            "focus must move off Approve for the round trip to prove a repaint:\n{}",
            self.terminal.screen_text()
        );
        self.terminal.send(b"\x1b[D");
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && !approve_is_focused(&self.terminal.poll_screen()) {
            thread::sleep(Duration::from_millis(20));
        }
        self.terminal.mark("focus-left");
        (before, self.settled_screen(timeout))
    }

    /// Drains until the screen stops changing, bounded by `timeout`.
    ///
    /// Comparing against a frame that is still arriving would report a
    /// difference that has nothing to do with the repaint under test.
    fn settled_screen(&mut self, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let mut previous = self.terminal.poll_screen();
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
            let current = self.terminal.poll_screen();
            if current == previous {
                return current;
            }
            previous = current;
        }
        previous
    }

    /// Resizes the terminal's row count and reports whether the worker surface was
    /// relinquished and exactly one card is left visible.
    ///
    /// A resize invalidates the rectangle's recorded coordinates, so the surface is
    /// given up *without* erasing those rows and the old viewport is pushed into
    /// scrollback: the invalidated card must not stay in the viewport reading as the
    /// active one, and the replacement inline card must be the only one there.
    /// Counting cards is what makes this discriminating - skipping the scrollback
    /// push would leave two.
    pub(crate) fn resize_rows_and_expect_one_inline_card(
        &mut self,
        rows: u16,
        timeout: Duration,
    ) -> bool {
        let cols = self.terminal.cols();
        self.resize_and_expect_one_inline_card(rows, cols, timeout)
    }

    pub(crate) fn resize_and_expect_one_inline_card(
        &mut self,
        rows: u16,
        cols: u16,
        timeout: Duration,
    ) -> bool {
        self.terminal.mark("old-card-before-resize");
        self.terminal.resize(rows, cols);
        let deadline = Instant::now() + timeout;
        loop {
            let workers_gone = self.live_worker_pids().is_empty();
            let screen = self.terminal.poll_screen();
            if workers_gone && screen.matches(APPROVE_LABEL).count() == 1 && screen.contains("Deny")
            {
                self.terminal.mark("resize-one-inline-card");
                return true;
            }
            if Instant::now() >= deadline {
                self.terminal.mark("resize-timeout");
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Narrows the terminal below the surface width and reports whether a card
    /// with Approve focused is visible again within `timeout`.
    ///
    /// The gate refuses a viewport narrower than the surface, so the slice must
    /// disable itself and hand the approval to the inline card in the same pass.
    /// If the claim that a card is already on screen were not released, the inline
    /// path would skip the request and nothing at all would be left on screen.
    pub(crate) fn narrow_below_surface_and_card_is_visible(
        &mut self,
        cols: u16,
        timeout: Duration,
    ) -> bool {
        let rows = self.terminal.rows();
        self.terminal.resize(rows, cols);
        let deadline = Instant::now() + timeout;
        loop {
            // The worker card is still on screen until the shell notices the
            // resize, so "a card is visible" on its own would pass before the
            // fallback happened and prove nothing. Disabling the slice shuts its
            // worker down, so no worker left *and* a card still visible is the
            // inline card having taken over.
            let workers_gone = self.live_worker_pids().is_empty();
            let screen = self.terminal.poll_screen();
            if workers_gone && screen.contains(APPROVE_LABEL) && screen.contains("Deny") {
                self.terminal.mark("narrow-inline-fallback");
                return true;
            }
            if Instant::now() >= deadline {
                self.terminal.mark("narrow-timeout");
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Live `cosh-ui-worker` descendants of the shell right now.
    pub(crate) fn live_worker_pids(&mut self) -> Vec<u32> {
        let shell_pid = self.terminal.child_pid();
        self.ledger.scan_tree(shell_pid);
        self.ledger.write();
        descendants(shell_pid)
            .into_iter()
            .filter(|pid| exe_name(*pid) == WORKER_EXE)
            .collect()
    }

    /// Sends the fault's signal to the shell's single live worker.
    ///
    /// The target is re-derived from `/proc` and re-checked against both the
    /// shell's descendant set and the executable name immediately before the
    /// signal, so a pid that was reused by an unrelated process cannot be hit.
    /// Returns the pid that was signalled.
    pub(crate) fn fault_worker(&mut self, fault: WorkerFault) -> u32 {
        let signal = fault.signal();
        let pid = self
            .worker_descendants_within(Duration::from_secs(5))
            .into_iter()
            .next()
            .expect("a live worker to fault");
        let shell_pid = self.terminal.child_pid();
        assert!(
            descendants(shell_pid).contains(&pid) && exe_name(pid) == WORKER_EXE,
            "refusing to signal pid {pid}: it is no longer this shell's worker"
        );
        assert_eq!(
            unsafe { libc::kill(i32::try_from(pid).expect("pid fits i32"), signal) },
            0,
            "kill worker {pid} with signal {signal}"
        );
        self.terminal.mark("worker-faulted");
        pid
    }

    /// Waits for a rebuilt worker: exactly one live worker whose pid is not the
    /// faulted one.
    ///
    /// This is what separates real recovery from a test that would pass anyway.
    /// The card painted by the faulted generation is still on screen, and the
    /// existing input capture still resolves the approval, so "the approval
    /// completes" alone cannot tell whether the Host repainted the card through a
    /// new worker or dropped the rebuilt frame and let the failure budget run out.
    pub(crate) fn wait_for_rebuilt_worker(
        &mut self,
        victim: u32,
        timeout: Duration,
    ) -> Option<u32> {
        let deadline = Instant::now() + timeout;
        loop {
            let workers = self.live_worker_pids();
            if workers.len() == 1 && workers[0] != victim {
                return Some(workers[0]);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until no `cosh-ui-worker` descendant is alive.
    ///
    /// A zombie still has a `/proc` entry, so this does not pass until every
    /// worker the session spawned has actually been reaped.
    pub(crate) fn wait_for_no_workers(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.live_worker_pids().is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until `pid` is gone from `/proc`, which is what proves the Host
    /// reaped it rather than leaving a zombie behind.
    pub(crate) fn wait_for_pid_gone(&mut self, pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.ledger.scan_tree(self.terminal.child_pid());
            if !process_alive(pid) {
                self.ledger.write();
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.ledger.write();
        false
    }

    /// Declines the pending card with Esc and exits the shell.
    pub(crate) fn deny_and_exit(&mut self) -> Option<i32> {
        self.terminal.send(b"\x1b");
        self.terminal
            .wait_for("after-deny", READY_MARKER, STEP_TIMEOUT);
        self.shutdown()
    }

    /// Declines the pending card, then records what the run actually did.
    ///
    /// The observation is what makes the deny path assertable rather than
    /// merely non-panicking: it carries the effect count, the audit lines, the
    /// surviving-worker count and the Bash facts, so "nothing ran and nothing
    /// was left behind" is checked instead of assumed.
    pub(crate) fn deny_and_observe(&mut self) -> ApprovalSliceObservation {
        self.resolve_and_observe_without_effect(b"\x1b")
    }

    pub(crate) fn select_deny_and_wait(&mut self) -> bool {
        self.terminal.send(b"\x1b[C");
        let deadline = Instant::now() + STEP_TIMEOUT;
        while Instant::now() < deadline {
            let screen = self.terminal.poll_screen();
            let flat = screen.split_whitespace().collect::<Vec<_>>().join(" ");
            if flat.contains("> [ Deny ]") {
                self.terminal.mark("deny-selected");
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    pub(crate) fn submit_with_enter_and_observe_noneffect(&mut self) -> ApprovalSliceObservation {
        self.resolve_and_observe_without_effect(b"\r")
    }

    fn resolve_and_observe_without_effect(&mut self, input: &[u8]) -> ApprovalSliceObservation {
        if self.fatal.is_some() {
            return self.observe(None);
        }
        self.terminal.send(input);
        if !self
            .terminal
            .wait_for("after-deny", READY_MARKER, STEP_TIMEOUT)
        {
            self.fail(OracleError::ScreenMissing {
                needle: READY_MARKER,
            });
        }
        self.terminal.mark("after-deny");
        let bash_changed = self.bash_drift();
        let exit_code = self.shutdown();
        self.observe(bash_changed).with_exit(exit_code)
    }

    pub(crate) fn approve_execute_shell_second_request_from_visible_card(
        &mut self,
    ) -> ApprovalSliceObservation {
        if self.fatal.is_some() {
            return self.observe(None);
        }
        self.terminal.send(b"\r");
        if !self.wait_effect_lines(1) {
            self.fail(OracleError::EffectCount {
                actual: self.effect_lines(),
            });
        }
        if !self
            .terminal
            .wait_for("first-done", "slice first done", STEP_TIMEOUT)
        {
            self.fail(OracleError::ScreenMissing {
                needle: "slice first done",
            });
        }
        self.terminal.mark("first-done");
        self.terminal.send_line("printf 'slice-ordinary-%s\\n' ok");
        if !self
            .terminal
            .wait_for("ordinary", "slice-ordinary-ok", STEP_TIMEOUT)
        {
            self.fail(OracleError::ScreenMissing {
                needle: "slice-ordinary-ok",
            });
        }
        self.terminal.mark("ordinary");
        self.terminal.send_line("?? slice second request");
        if !self
            .terminal
            .wait_for("second", "slice second response", STEP_TIMEOUT)
        {
            self.fail(OracleError::ScreenMissing {
                needle: "slice second response",
            });
        }
        self.terminal.mark("second");
        let bash_changed = self.bash_drift();
        let exit_code = self.shutdown();
        self.observe(bash_changed).with_exit(exit_code)
    }

    fn bash_drift(&mut self) -> Option<String> {
        let facts = self.bash.as_ref()?;
        if !process_alive(facts.pid) {
            return Some(format!("bash pid {} is gone", facts.pid));
        }
        let ticks = start_ticks(facts.pid);
        if ticks != facts.start_ticks {
            return Some(format!(
                "bash pid {} start ticks changed {ticks} != {}",
                facts.pid, facts.start_ticks
            ));
        }
        let cwd = read_link(&format!("/proc/{}/cwd", facts.pid));
        if cwd != facts.cwd {
            return Some(format!("bash cwd changed {cwd} != {}", facts.cwd));
        }
        if !facts.background_job.is_empty() {
            let alive = direct_children(facts.pid)
                .into_iter()
                .any(|pid| exe_name(pid) == "sleep");
            if !alive {
                return Some(format!(
                    "background job {} is no longer running under bash",
                    facts.background_job
                ));
            }
        }
        None
    }

    fn shutdown(&mut self) -> Option<i32> {
        self.terminal.send_line("exit");
        let status = self.terminal.wait_exit(EXIT_TIMEOUT);
        self.ledger.scan_tree(self.terminal.child_pid());
        self.ledger.write();
        status
    }

    fn effect_lines(&self) -> usize {
        fs::read_to_string(&self.effect_file)
            .map(|body| body.lines().filter(|line| *line == EFFECT_LINE).count())
            .unwrap_or(0)
    }

    fn wait_effect_lines(&mut self, expected: usize) -> bool {
        let deadline = Instant::now() + STEP_TIMEOUT;
        while Instant::now() < deadline {
            if self.effect_lines() == expected {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn observe(&mut self, bash_changed: Option<String>) -> ApprovalSliceObservation {
        self.ledger.scan_tree(self.terminal.child_pid());
        self.ledger.write();
        let worker_descendants = self.ledger.count_role("cosh-ui-worker");
        let mut failures = std::mem::take(&mut self.failures);
        if let Some(fatal) = self.fatal.clone() {
            failures.insert(0, fatal);
        }
        if let Some(detail) = bash_changed.clone() {
            failures.push(OracleError::BashFactChanged { detail });
        }
        let audit = read_audit_lines(self.root.path());
        let diagnostics = build_diagnostics(&audit, self.root.path());
        let observation = ApprovalSliceObservation {
            http_bodies: self.server.bodies(),
            effect_count: self.effect_lines(),
            audit,
            transcript: self.terminal.raw_transcript(),
            timeline: self.terminal.take_timeline(),
            ledger: self.ledger.snapshot(),
            screen: self.terminal.screen_text(),
            diagnostics,
            core_sha256: self.core_sha256.clone(),
            worker_descendants,
            bash_changed,
            exit_code: None,
            failures,
        };
        if let Some(directory) = std::env::var_os("COSH_UI_EVIDENCE_DIR") {
            let directory = PathBuf::from(directory).join(format!(
                "{}-{}",
                std::process::id(),
                SLICE_LABEL_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&directory).expect("create supplemental replay evidence");
            fs::write(directory.join("transcript.bin"), &self.terminal.raw)
                .expect("write original PTY bytes");
            let metadata = serde_json::json!({
                "execution": "supplemental-emulator-replay",
                "test_name": std::thread::current().name().map(str::to_owned),
                "timing": "synthetic; original geometry byte offsets preserved",
                "geometry": self.terminal.geometry,
                "shell_sha256": self.terminal.binary_sha256,
                "worker_sha256": self.terminal.worker_sha256,
                "core_sha256": observation.core_sha256,
                "timeline": observation.timeline.iter().map(|entry| serde_json::json!({"label": entry.label, "offset": entry.offset, "row": entry.row, "col": entry.col})).collect::<Vec<_>>(),
                "bash_baseline": self.bash.as_ref().map(|facts| serde_json::json!({"pid": facts.pid, "start_ticks": facts.start_ticks, "cwd": facts.cwd, "background_job": facts.background_job})),
                "source_head": std::env::var("COSH_UI_EVIDENCE_HEAD").ok(),
                "source_tree": std::env::var("COSH_UI_EVIDENCE_SOURCE_TREE").ok(),
                "effect_count": observation.effect_count,
                "stderr_logging_visible": self.stderr_logging_visible(),
                "audit": observation.audit,
                "ledger": observation.ledger,
                "bash_changed": observation.bash_changed,
                "diagnostics": observation.diagnostics,
            });
            fs::write(directory.join("metadata.json"), metadata.to_string())
                .expect("write supplemental replay metadata");
        }
        observation
    }

    #[allow(dead_code)]
    pub(crate) fn host_log_markers(&self) -> &HostLogMarkers {
        &self.markers
    }
}

impl ApprovalSliceObservation {
    fn with_exit(mut self, exit_code: Option<i32>) -> Self {
        self.exit_code = exit_code;
        if exit_code != Some(0)
            && !self
                .failures
                .iter()
                .any(|failure| matches!(failure, OracleError::ExitStatus { .. }))
        {
            self.failures
                .push(OracleError::ExitStatus { code: exit_code });
        }
        self
    }
}

impl Drop for RealCoreApprovalHarness {
    fn drop(&mut self) {
        // Bash job control puts a background job in its own process group, so
        // the terminal's `kill(-pid)` cannot reach the `sleep 600` fixture, and
        // that job's cwd is the temp HOME. On overlayfs a directory still in
        // use as a cwd cannot be removed, which leaked the whole fixture root
        // (observed: cleanup probe `tmp_roots_present` non-empty). Reap every
        // recorded process by (pid, start_ticks) before TempDir drops.
        self.ledger.kill_recorded();
        self.ledger.write();
    }
}

struct BashFacts {
    pid: u32,
    start_ticks: u64,
    cwd: String,
    background_job: String,
}

pub(crate) fn real_cosh_core_path() -> PathBuf {
    if let Some(profile) = std::env::var_os("COSH_ACCEPT_RELEASE_PROFILE") {
        let core = PathBuf::from(profile).join("cosh-core");
        assert!(core.is_file(), "missing release Core: {}", core.display());
        return core;
    }
    let core = std::env::current_exe()
        .expect("current test binary")
        .parent()
        .expect("deps directory")
        .parent()
        .expect("target profile directory")
        .join("cosh-core");
    assert!(
        core.is_file(),
        "missing real cosh-core artifact at {}; run cargo build --locked -p cosh-core --bin cosh-core",
        core.display()
    );
    core
}

fn assert_real_core(path: &Path) -> Result<String, OracleError> {
    let bytes = fs::read(path).map_err(|_| OracleError::CoreNotElf)?;
    if bytes.len() < 4 || &bytes[..4] != b"\x7fELF" {
        return Err(OracleError::CoreNotElf);
    }
    Ok(sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A `#!/bin/sh` stand-in for cosh-core. It is not an ELF image, so the AB-06
/// identity oracle must reject it before any provider traffic is attributed to
/// a real Core.
pub(crate) fn write_script_core_for_mutation() -> PathBuf {
    let directory = Path::new(EVIDENCE_ROOT).join(format!("script-core-{}", std::process::id()));
    fs::create_dir_all(&directory).expect("script core dir");
    let path = directory.join("cosh-core");
    fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write script core");
    set_executable(&path);
    path
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .expect("script core metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod script core");
}

pub(crate) fn shell_quote(value: &OsStr) -> String {
    let text = value.to_str().expect("utf-8 test path");
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn sse(chunks: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str("data: ");
        body.push_str(&chunk.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn tool_call_response(effect_file: &Path, delay: Option<u64>) -> String {
    let command = format!(
        "printf '%s\\n' {EFFECT_LINE} >> {}",
        shell_quote(effect_file.as_os_str())
    );
    let command = if let Some(delay) = delay {
        format!("sleep {delay}; {command}")
    } else {
        command
    };
    let arguments = serde_json::json!({ "command": command }).to_string();
    sse(&[
        serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":TOOL_CALL_ID,"type":"function","function":{"name":"shell","arguments":arguments}}]}}]}),
        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
    ])
}

fn text_response(text: &str) -> String {
    sse(&[
        serde_json::json!({"choices":[{"index":0,"delta":{"content":text}}]}),
        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ])
}

struct ServerState {
    bodies: Vec<serde_json::Value>,
}

pub(crate) struct DeterministicOpenAiServer {
    state: Arc<Mutex<ServerState>>,
    shutdown: Arc<AtomicBool>,
}

impl DeterministicOpenAiServer {
    fn start(script: ServerScript, effect_file: PathBuf) -> (Self, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback provider");
        listener
            .set_nonblocking(true)
            .expect("nonblocking provider listener");
        let port = listener.local_addr().expect("provider address").port();
        let state = Arc::new(Mutex::new(ServerState { bodies: Vec::new() }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_shutdown = shutdown.clone();
        thread::spawn(move || {
            while !worker_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let index = {
                            let guard = worker_state.lock().expect("provider state");
                            guard.bodies.len()
                        };
                        serve(stream, index, script, &effect_file, &worker_state);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (Self { state, shutdown }, port)
    }

    pub(crate) fn bodies(&self) -> Vec<serde_json::Value> {
        self.state.lock().expect("provider state").bodies.clone()
    }
}

impl Drop for DeterministicOpenAiServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn serve(
    mut stream: std::net::TcpStream,
    index: usize,
    script: ServerScript,
    effect_file: &Path,
    state: &Arc<Mutex<ServerState>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("provider read timeout");
    let Some(body) = read_http_body(&mut stream) else {
        return;
    };
    let mut parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    if index == 1 && script == ServerScript::OmitHostResult {
        if let Some(messages) = parsed
            .get_mut("messages")
            .and_then(|value| value.as_array_mut())
        {
            messages.retain(|message| {
                message.get("role").and_then(|role| role.as_str()) != Some("tool")
            });
        }
    }
    state.lock().expect("provider state").bodies.push(parsed);
    let payload = match index {
        0 => tool_call_response(
            effect_file,
            match script {
                ServerScript::DelayedEffect => Some(1),
                ServerScript::TimeoutEffect => Some(30),
                _ => None,
            },
        ),
        1 => text_response("slice first done"),
        _ => text_response("slice second response"),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    let _ = stream.flush();
}

fn read_http_body(stream: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    let mut received = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        received.extend_from_slice(&chunk[..count]);
        if let Some(index) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&received[..header_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    })?;
    while received.len() < header_end + content_length {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        received.extend_from_slice(&chunk[..count]);
    }
    Some(received[header_end..header_end + content_length].to_vec())
}

pub(crate) struct SliceTerminal {
    child: Child,
    master: File,
    _slave: File,
    parser: vt100::Parser,
    raw: Vec<u8>,
    geometry: Vec<(usize, u16, u16)>,
    binary_sha256: Option<String>,
    worker_sha256: Option<String>,
    action_start: usize,
    last_output: Instant,
    timeline: Vec<TimelineEntry>,
    cols: u16,
    rows: u16,
    /// Only the worker launch asks the terminal for a cursor position report.
    /// Answering on the inline launch would write `ESC[<row>;<col>R` back into
    /// the shell's input stream, which the raw input layer consumes as keys and
    /// which displaces the active agent run — the approval request then fails
    /// the `foreign_to_active_run` gate and is denied by the batch drain.
    answer_cpr: bool,
}

impl SliceTerminal {
    fn spawn(launch: &SliceLaunch, core: &Path, port: u16, home: &Path) -> Self {
        let size = libc::winsize {
            ws_row: launch.rows,
            ws_col: launch.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(Some(&size), None).expect("slice PTY");
        let master = File::from(pty.master);
        let slave = File::from(pty.slave);
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "read slice master flags");
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0,
            "set slice master nonblocking"
        );
        let binary = match &launch.worker_profile {
            Some(profile) => profile.join("cosh-shell"),
            None => PathBuf::from(env!("CARGO_BIN_EXE_cosh-shell")),
        };
        let binary_sha256 = std::env::var_os("COSH_UI_EVIDENCE_DIR").map(|_| {
            format!(
                "{:x}",
                Sha256::digest(fs::read(&binary).expect("read shell binary"))
            )
        });
        let mut command = Command::new(binary);
        configure_raw_cli_command(&mut command);
        command.env_remove("COSH_ACCEPT_RELEASE_PROFILE");
        if !launch.activation {
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("COSH_UI_") {
                    command.env_remove(key);
                }
            }
        }
        if launch.output_fault_library.is_some() {
            command
                .env_remove("COSH_SHELL_ISOLATED")
                .env_remove("COSH_SHELL_INTEGRATION");
        }
        command
            .args(["raw", "cosh-core"])
            .env("HOME", home)
            .env("TMPDIR", home)
            .env("COSH_CORE_PATH", core)
            .env("COSH_SHELL_WIDTH", launch.cols.to_string())
            .env("COSH_AI_PROVIDER", "ui-approval-slice")
            .env("COSH_MODEL", "ui-approval-slice-model")
            .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}/v1"))
            .env("OPENAI_API_KEY", "test-only")
            .env("COSH_SHELL_APPROVAL_MODE", launch.approval_mode)
            .env("COSH_APPROVAL_MODE", launch.approval_mode)
            .env("COSH_LOG", "debug")
            .env("TERM", "xterm-256color")
            .env_remove("DASHSCOPE_API_KEY")
            .env_remove("ALIBABA_CLOUD_ACCESS_KEY_ID")
            .env_remove("ALIBABA_CLOUD_ACCESS_KEY_SECRET")
            .env_remove("ALIBABA_CLOUD_SECURITY_TOKEN")
            .env_remove("COSH_SHELL_STATUS_SYMBOLS")
            .current_dir(home)
            .stdin(Stdio::from(slave.try_clone().expect("clone slave stdin")))
            .stdout(Stdio::from(slave.try_clone().expect("clone slave stdout")))
            .stderr(Stdio::from(slave.try_clone().expect("clone slave stderr")));
        // No `process_group(0)` here: it makes the child a group leader before
        // `pre_exec` runs, and `setsid()` then fails with EPERM by POSIX rule.
        // `setsid()` alone already gives the child its own process group, which
        // is what the Drop handler's `kill(-pid)` needs.
        let mut worker_sha256 = launch.worker_profile.as_ref().and_then(|profile| {
            std::env::var_os("COSH_UI_EVIDENCE_DIR")?;
            fs::read(profile.join(WORKER_EXE))
                .ok()
                .map(|bytes| sha256_hex(&bytes))
        });
        if launch.activation {
            // Spec v2 §4: activation needs every test policy key; a missing one
            // keeps the inline path, which would turn a wiring RED into a
            // configuration RED.
            let worker = launch
                .worker_binary
                .clone()
                .unwrap_or_else(sibling_worker_path);
            if std::env::var_os("COSH_UI_EVIDENCE_DIR").is_some() {
                worker_sha256 = fs::read(&worker)
                    .ok()
                    .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
            }
            command
                .env("COSH_UI_APPROVAL_SLICE_TEST", "1")
                .env("COSH_SHELL_WIDTH", "40")
                .env("COSH_UI_WORKER_PATH", worker);
            for (key, value) in SLICE_TEST_POLICY {
                command.env(key, value);
            }
        }
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0
                    || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if let Some(timeout) = launch.handoff_timeout_secs {
            command.env("COSH_SHELL_HANDOFF_TIMEOUT_SECS", timeout.to_string());
        }
        if let Some(library) = &launch.output_fault_library {
            fs::write(home.join("output-arm"), b"armed").expect("owned output arm");
            command
                .env(
                    "COSH_UI_OUTPUT_TRIGGER",
                    if launch.output_fault_before_receipt {
                        "withdraw"
                    } else {
                        "receipt"
                    },
                )
                .env("LD_PRELOAD", library)
                .env("COSH_UI_OUTPUT_ARM", home.join("output-arm"))
                .env("COSH_UI_OUTPUT_HIT", home.join("output-hit"))
                .env("COSH_UI_STAGE_HIT", home.join("stage-hit"))
                .env("COSH_UI_STAGE_RELEASE", home.join("stage-release"));
        }
        let child = command.spawn().expect("spawn cosh-shell raw cosh-core");
        Self {
            child,
            master,
            _slave: slave,
            parser: vt100::Parser::new(launch.rows, launch.cols, 0),
            raw: Vec::new(),
            geometry: vec![(0, launch.rows, launch.cols)],
            binary_sha256,
            worker_sha256,
            action_start: 0,
            last_output: Instant::now(),
            timeline: Vec::new(),
            cols: launch.cols,
            rows: launch.rows,
            answer_cpr: launch.activation,
        }
    }

    pub(crate) fn child_pid(&self) -> u32 {
        self.child.id()
    }

    #[allow(dead_code)]
    pub(crate) fn cols(&self) -> u16 {
        self.cols
    }

    pub(crate) fn rows(&self) -> u16 {
        self.rows
    }

    /// Resizes the PTY, which delivers SIGWINCH to the shell.
    ///
    /// The parser geometry is changed before the shell's response is consumed, so
    /// output produced at the old size is never replayed at the new one.
    pub(crate) fn resize(&mut self, rows: u16, cols: u16) {
        let unchanged = (rows, cols) == (self.rows, self.cols);
        self.geometry.push((self.raw.len(), rows, cols));
        self.action_start = self.raw.len();
        self.parser.screen_mut().set_size(rows, cols);
        self.cols = cols;
        self.rows = rows;
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0,
            "resize slice PTY"
        );
        if unchanged {
            assert_eq!(
                unsafe { libc::kill(self.child_pid() as i32, libc::SIGWINCH) },
                0,
                "same-size resize notification"
            );
        }
    }

    pub(crate) fn screen_text(&self) -> String {
        self.parser.screen().contents().trim_end().to_string()
    }

    /// Drains whatever the shell has written and returns the screen.
    ///
    /// `screen_text` reports the last drained byte, so a poll loop built on it
    /// alone never observes a repaint and always times out.
    pub(crate) fn poll_screen(&mut self) -> String {
        self.read_output();
        self.screen_text()
    }

    pub(crate) fn raw_transcript(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }

    pub(crate) fn take_timeline(&mut self) -> Vec<TimelineEntry> {
        std::mem::take(&mut self.timeline)
    }

    pub(crate) fn mark(&mut self, label: &'static str) {
        let (row, col) = self.parser.screen().cursor_position();
        self.timeline.push(TimelineEntry {
            label,
            offset: self.raw.len(),
            row,
            col,
        });
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) {
        self.action_start = self.raw.len();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut written = 0;
        while written < bytes.len() {
            assert!(Instant::now() < deadline, "slice PTY write timeout");
            match self.master.write(&bytes[written..]) {
                Ok(0) => panic!("slice PTY write closed"),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.read_output(),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("slice PTY write: {error}"),
            }
        }
    }

    pub(crate) fn send_line(&mut self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\r');
        self.send(&bytes);
    }

    pub(crate) fn wait_for(
        &mut self,
        label: &'static str,
        needle: &str,
        timeout: Duration,
    ) -> bool {
        self.wait_for_all(label, &[needle], timeout)
    }

    pub(crate) fn wait_for_all(
        &mut self,
        label: &'static str,
        needles: &[&str],
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.read_output();
            let screen = self.screen_text();
            if self.raw.len() > self.action_start
                && self.last_output.elapsed() >= QUIET
                && needles.iter().all(|needle| screen.contains(needle))
            {
                self.mark(label);
                return true;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "{label}: timeout waiting for {needles:?}\nscreen:\n{screen}\ntranscript tail:\n{}",
                    &self.raw_transcript()[self.raw_transcript().len().saturating_sub(2000)..]
                );
                return false;
            }
        }
    }

    pub(crate) fn capture_pattern(
        &mut self,
        label: &'static str,
        prefix: &str,
        timeout: Duration,
    ) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            self.read_output();
            let screen = self.screen_text();
            if let Some(start) = screen.rfind(prefix) {
                let tail = &screen[start + prefix.len()..];
                let digits: String = tail.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    self.mark(label);
                    return Some(digits);
                }
            }
            if Instant::now() >= deadline {
                eprintln!("{label}: timeout capturing {prefix:?}; screen:\n{screen}");
                return None;
            }
        }
    }

    fn read_output(&mut self) {
        let mut poll = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 10) };
        if result < 0 {
            let error = io::Error::last_os_error();
            // SIGCHLD from a reaped descendant interrupts poll; that carries no
            // information about the PTY, so retry instead of failing the run.
            if error.kind() == io::ErrorKind::Interrupted {
                return;
            }
            panic!("poll slice PTY: {error}");
        }
        if result == 0 {
            return;
        }
        let mut bytes = [0u8; 8192];
        match self.master.read(&mut bytes) {
            Ok(0) => {}
            Ok(count) => {
                let chunk = &bytes[..count];
                self.raw.extend_from_slice(chunk);
                self.parser.process(chunk);
                self.last_output = Instant::now();
                if self.answer_cpr && chunk.windows(3).any(|window| window == b"\x1b[6n") {
                    self.answer_cursor_position();
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => panic!("slice PTY read: {error}"),
        }
    }

    /// The worker path asks the terminal where the cursor is; the inline path
    /// never does. Answering keeps both launches on one terminal implementation.
    fn answer_cursor_position(&mut self) {
        let (row, col) = self.parser.screen().cursor_position();
        let reply = format!("\x1b[{};{}R", row + 1, col + 1);
        let _ = self.master.write_all(reply.as_bytes());
        let _ = self.master.flush();
    }

    pub(crate) fn wait_exit(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            self.read_output();
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.code();
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }
}

impl Drop for SliceTerminal {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGTERM);
            }
            if self
                .child
                .wait_timeout(Duration::from_secs(2))
                .ok()
                .flatten()
                .is_none()
            {
                unsafe {
                    libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                }
                let _ = self.child.wait_timeout(Duration::from_secs(2));
            }
        }
    }
}

/// How the harness faults a live worker.
///
/// Signals stay inside the support module so a test names the failure it means
/// rather than a number, and so no test crate needs a `libc` dependency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerFault {
    /// Kill outright: the process disappears without a shutdown handshake.
    Kill,
    /// Stop it: nothing exits, so only the heartbeat timeout can notice.
    Hang,
}

impl WorkerFault {
    fn signal(self) -> i32 {
        match self {
            Self::Kill => libc::SIGKILL,
            Self::Hang => libc::SIGSTOP,
        }
    }
}

pub(crate) fn test_worker_profile(worker: &Path) -> tempfile::TempDir {
    let release = std::env::var_os("COSH_ACCEPT_RELEASE_PROFILE").map(PathBuf::from);
    let profile = match &release {
        Some(_) => tempfile::tempdir_in(std::env::var_os("HOME").expect("secure profile HOME")),
        None => tempfile::tempdir(),
    }
    .expect("isolated same-build worker profile");
    let source = release
        .as_ref()
        .map(|path| path.join("cosh-shell"))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_cosh-shell")));
    let shell = profile.path().join("cosh-shell");
    fs::copy(&source, &shell).expect("copy real shell into test profile");
    if let Some(release) = release {
        fs::copy(release.join("cosh-core"), profile.path().join("cosh-core"))
            .expect("copy release Core into test profile");
    }
    fs::copy(worker, profile.path().join(WORKER_EXE)).expect("copy injected sibling worker");
    profile
}

/// A worker candidate that handshakes correctly and then breaks the protocol.
///
/// The handshake is valid on purpose: what must be refused is a frame the Host
/// cannot decode, arriving from a worker that already proved it could speak v2.
/// A candidate that failed the handshake instead would only re-test version
/// rejection.
pub(crate) fn write_malformed_worker() -> PathBuf {
    let directory = Path::new(EVIDENCE_ROOT).join(format!(
        "malformed-worker-{}-{}",
        std::process::id(),
        SLICE_LABEL_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).expect("create malformed worker dir");
    let path = directory.join(WORKER_EXE);
    fs::write(
        &path,
        r##"#!/usr/bin/python3
import json
import os
import socket
import struct
import sys

args = dict(zip(iter(sys.argv[1::2]), iter(sys.argv[2::2])))
sock = socket.socket(fileno=int(args['--ipc-fd']))


def read_exact(size):
    data = b''
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            return None
        data += chunk
    return data


def receive():
    prefix = read_exact(4)
    if prefix is None:
        return None
    frame = read_exact(struct.unpack('<I', prefix)[0])
    if frame is None:
        return None
    generation, frame_id = struct.unpack('<QQ', frame[27:43])
    return frame[11:27], generation, frame_id, json.loads(frame[43:].decode())


def send(message_type, nonce, generation, frame_id, payload):
    body = json.dumps(payload, separators=(',', ':')).encode()
    header = b'CUI1' + struct.pack('<HBI', 2, message_type, len(body))
    header += nonce + struct.pack('<QQ', generation, frame_id)
    sock.sendall(struct.pack('<I', len(header) + len(body)) + header + body)


handshake = receive()
if handshake is None:
    raise SystemExit(33)
nonce, generation, frame_id, hello = handshake
send(4, nonce, generation, frame_id, {
    'protocol_version': 2,
    'candidate_version': hello['candidate_version'],
    'capabilities': ['typed-draw-ops-v1'],
})
render = receive()
if render is not None:
    if 'viewport' not in render[3]:
        raise SystemExit(34)
    # Not a frame: a length prefix that promises more bytes than follow, then
    # bytes that are not a valid header at all.
    sock.sendall(struct.pack('<I', 4096) + bytes([255, 254, 253, 252]) * 16)
    with open(os.path.join(os.path.dirname(__file__), 'malformed-proof.json'), 'w') as proof:
        json.dump({'protocol_version': hello['protocol_version'], 'candidate_version': hello['candidate_version'], 'render_received': True, 'wire_bytes': 68}, proof)
raise SystemExit(0)
"##,
    )
    .expect("write malformed worker");
    set_executable(&path);
    path
}

fn sibling_worker_path() -> PathBuf {
    let worker = PathBuf::from(env!("CARGO_BIN_EXE_cosh-ui-worker"));
    assert!(
        worker.is_file(),
        "Cargo worker artifact missing: {}",
        worker.display()
    );
    worker
}

pub(crate) struct HostLogMarkers {
    path: PathBuf,
}

impl HostLogMarkers {
    fn new(home: &Path, shell_pid: u32) -> Self {
        Self {
            path: home
                .join(".copilot-shell/logs")
                .join(format!("ui-approval-slice-stderr.{shell_pid}.log")),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Absent file means "no marker yet", never an error; every wait is bounded.
    #[allow(dead_code)]
    pub(crate) fn wait_for(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(body) = fs::read_to_string(&self.path) {
                if body.contains(needle) {
                    return true;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

#[derive(Clone, Debug)]
struct LedgerProcess {
    role: String,
    pid: u32,
    start_ticks: u64,
}

pub(crate) struct ProcessLedger {
    path: PathBuf,
    processes: Vec<LedgerProcess>,
    socket_inodes: BTreeSet<u64>,
    tmp_roots: Vec<String>,
    worker_identities: Vec<serde_json::Value>,
}

static SLICE_LABEL_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Per-harness evidence directory label.
///
/// libtest names each test thread after its test, which is the only portable
/// per-test key available here: under `--test-threads=1` every slice test shares
/// one process pid, so a pid-only directory got overwritten and the cleanup probe
/// saw fewer ledgers than harnesses. The thread name alone still collides when one
/// test spawns two harnesses, as the inline-versus-worker deny comparison does, so
/// a monotonic sequence separates them and every harness gets its own ledger.
fn slice_label() -> String {
    let sequence = SLICE_LABEL_SEQ.fetch_add(1, Ordering::Relaxed);
    // Bound the `Thread` handle first: `name()` borrows it, so chaining straight
    // off `thread::current()` would drop the temporary while still borrowed.
    let current = thread::current();
    let name = current.name().unwrap_or("slice");
    format!("{}-{sequence}", name.replace("::", ".").replace('/', "_"))
}

impl ProcessLedger {
    fn new(label: &str, root: &Path) -> Self {
        let directory = Path::new(EVIDENCE_ROOT).join(format!("{label}-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("create slice evidence dir");
        Self {
            path: directory.join("ledger.json"),
            processes: Vec::new(),
            socket_inodes: BTreeSet::new(),
            tmp_roots: vec![root.display().to_string()],
            worker_identities: Vec::new(),
        }
    }

    fn record(&mut self, role: &str, pid: u32) {
        if !self.processes.iter().any(|entry| entry.pid == pid) {
            self.processes.push(LedgerProcess {
                role: role.to_string(),
                pid,
                start_ticks: start_ticks(pid),
            });
            self.socket_inodes.extend(socket_inodes(pid));
        }
    }

    fn scan_tree(&mut self, root_pid: u32) {
        for pid in descendants(root_pid) {
            let role = exe_name(pid);
            self.record(&role, pid);
        }
        for entry in &self.processes {
            self.socket_inodes.extend(socket_inodes(entry.pid));
        }
    }

    fn count_role(&self, role: &str) -> usize {
        self.processes
            .iter()
            .filter(|entry| entry.role == role && process_alive(entry.pid))
            .count()
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "processes": self.processes.iter().map(|entry| serde_json::json!({
                "role": entry.role,
                "pid": entry.pid,
                "start_ticks": entry.start_ticks,
            })).collect::<Vec<_>>(),
            "socket_inodes": self.socket_inodes.iter().copied().collect::<Vec<_>>(),
            "tmp_roots": self.tmp_roots,
            "worker_identities": self.worker_identities,
        })
    }

    fn write(&self) {
        let tmp = self.path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(&self.snapshot()).expect("encode ledger"),
        )
        .expect("write slice ledger");
        fs::rename(&tmp, &self.path).expect("publish slice ledger");
    }

    /// TERM then KILL every recorded process, gated on `start_ticks` so a
    /// recycled pid can never be signalled. Descendants reparent to the
    /// container init, which reaps them; the cleanup probe asserts no zombie
    /// and no leftover fixture root survives.
    fn kill_recorded(&self) {
        let alive = |entry: &LedgerProcess| {
            process_alive(entry.pid) && start_ticks(entry.pid) == entry.start_ticks
        };
        for entry in self.processes.iter().filter(|entry| alive(entry)) {
            unsafe {
                libc::kill(entry.pid as i32, libc::SIGTERM);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline && self.processes.iter().any(alive) {
            thread::sleep(Duration::from_millis(20));
        }
        for entry in self.processes.iter().filter(|entry| alive(entry)) {
            unsafe {
                libc::kill(entry.pid as i32, libc::SIGKILL);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline && self.processes.iter().any(alive) {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn start_ticks(pid: u32) -> u64 {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    let Some(index) = stat.rfind(')') else {
        return 0;
    };
    stat[index + 2..]
        .split_whitespace()
        .nth(19)
        .and_then(|field| field.parse().ok())
        .unwrap_or(0)
}

fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn read_link(path: &str) -> String {
    fs::read_link(path)
        .map(|target| target.display().to_string())
        .unwrap_or_default()
}

fn exe_name(pid: u32) -> String {
    Path::new(&read_link(&format!("/proc/{pid}/exe")))
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn direct_children(pid: u32) -> Vec<u32> {
    // Scan `/proc/*/status` for PPid rather than reading
    // `/proc/<pid>/task/<tid>/children`: the latter lists only one thread's
    // children and needs CONFIG_PROC_CHILDREN, and it returned nothing for a
    // shell that demonstrably had bash running under it.
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(candidate) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if parent_pid(candidate) == Some(pid) {
            out.push(candidate);
        }
    }
    out.sort_unstable();
    out
}

fn parent_pid(pid: u32) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == "PPid")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

fn descendants(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for child in direct_children(pid) {
        out.push(child);
        out.extend(descendants(child));
    }
    out
}

fn descendant_with_name(pid: u32, name: &str) -> Option<u32> {
    descendants(pid)
        .into_iter()
        .find(|child| exe_name(*child) == name)
}

fn socket_inodes(pid: u32) -> BTreeSet<u64> {
    let mut inodes = BTreeSet::new();
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return inodes;
    };
    for entry in entries.flatten() {
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let text = target.to_string_lossy();
        if let Some(rest) = text.strip_prefix("socket:[") {
            if let Ok(inode) = rest.trim_end_matches(']').parse::<u64>() {
                inodes.insert(inode);
            }
        }
    }
    inodes
}

fn read_audit_lines(home: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let mut stack = vec![home.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let in_segments = path
                .components()
                .any(|component| component.as_os_str() == "segments");
            let is_jsonl = path
                .file_name()
                .map(|name| name.to_string_lossy().contains(".jsonl"))
                .unwrap_or(false);
            if in_segments && is_jsonl {
                if let Ok(body) = fs::read_to_string(&path) {
                    lines.extend(body.lines().map(str::to_string));
                }
            }
        }
    }
    lines
}

fn build_diagnostics(audit: &[String], home: &Path) -> String {
    let mut out = String::new();
    out.push_str("--- approval/tool audit records ---\n");
    for line in audit {
        if line.contains("\"approval.") || line.contains("\"tool.") {
            out.push_str(line);
            out.push('\n');
        }
    }
    let logs = home.join(".copilot-shell/logs");
    if let Ok(entries) = fs::read_dir(&logs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let body = fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = body.lines().collect();
            let tail_start = lines.len().saturating_sub(60);
            out.push_str(&format!(
                "--- {} ({} lines) ---\n",
                path.display(),
                lines.len()
            ));
            for line in &lines[tail_start..] {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

fn count_audit(audit: &[String], event: &str) -> usize {
    let needle = format!("\"event_type\":\"{event}\"");
    audit.iter().filter(|line| line.contains(&needle)).count()
}

fn audit_event_types(lines: &[String]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(event) = value.get("event_type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        match counts.iter_mut().find(|(name, _)| name == event) {
            Some(entry) => entry.1 += 1,
            None => counts.push((event.to_string(), 1)),
        }
    }
    counts
        .into_iter()
        .map(|(name, count)| format!("{name}={count}"))
        .collect()
}

fn approve_is_focused(screen: &str) -> bool {
    let flat: String = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    let (Some(approve), Some(deny), Some(details)) = (
        flat.find(APPROVE_LABEL),
        flat.find("Deny"),
        flat.find("Details"),
    ) else {
        return false;
    };
    approve < deny && deny < details && flat[..approve].chars().rev().take(8).any(|ch| ch == '>')
}

fn production_worker_identity(pid: u32, shell_pid: u32, profile: &Path) -> serde_json::Value {
    let executable = fs::read_link(format!("/proc/{pid}/exe")).expect("worker executable");
    assert_eq!(executable, profile.join(WORKER_EXE));
    let bytes = fs::read(&executable).expect("release worker ELF");
    assert!(bytes.starts_with(b"\x7fELF"));
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("worker stat");
    let (_, tail) = stat.rsplit_once(") ").expect("worker stat fields");
    let fields: Vec<_> = tail.split_whitespace().collect();
    assert_eq!(fields[1].parse::<u32>().expect("worker parent"), shell_pid);
    assert_eq!(fields[2].parse::<u32>().expect("worker group"), pid);
    assert_eq!(fields[3].parse::<u32>().expect("worker session"), pid);
    assert_eq!(
        fs::read_link(format!("/proc/{pid}/cwd")).expect("worker cwd"),
        Path::new("/")
    );
    assert!(fs::read(format!("/proc/{pid}/environ"))
        .expect("worker environment")
        .is_empty());
    let mut descriptors = std::collections::BTreeMap::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd")).expect("worker descriptors") {
        let entry = entry.expect("worker descriptor entry");
        let fd = entry
            .file_name()
            .to_string_lossy()
            .parse::<u32>()
            .expect("descriptor number");
        descriptors.insert(fd, fs::read_link(entry.path()).expect("descriptor target"));
    }
    assert_eq!(
        descriptors.keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    for fd in 0..3 {
        assert_eq!(descriptors[&fd], Path::new("/dev/null"));
    }
    assert!(descriptors[&3].to_string_lossy().starts_with("socket:["));
    serde_json::json!({"pid": pid, "parent": shell_pid, "session": pid, "cwd": "/", "environment_empty": true, "exe": executable, "sha256": sha256_hex(&bytes), "fds": descriptors})
}
