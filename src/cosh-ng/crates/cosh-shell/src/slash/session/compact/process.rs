//! Compactor subprocess: spawn, child lifecycle, stdout parsing, termination.
//!
//! The child handle lives in a single synchronized owner. Signals are only
//! ever sent while the un-reaped child is held, and reaping happens through
//! `poll`/`Drop`, never on a side thread, so a signal can never target a
//! recycled PID.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::adapter::{terminate_and_reap_process, terminate_process_group, StderrTail};

/// Bytes of compactor stdout retained for result parsing.
pub(super) const MAX_COMPACTOR_OUTPUT_BYTES: u64 = 256 * 1024;
/// Bytes of compactor stderr retained for failure diagnostics.
const MAX_COMPACTOR_STDERR_BYTES: usize = 4 * 1024;
/// Characters of an error message surfaced to the user.
pub(crate) const MAX_REPORTED_ERROR_CHARS: usize = 400;

/// Total wall-clock budget for one background compactor process.
///
/// Generous enough for a slow provider to stream a full structured summary,
/// but finite: a hung provider, network, cosh-core, or descendant process
/// must never keep the Agent conversation paused forever.
pub(super) const COMPACTOR_DEADLINE: Duration = Duration::from_secs(600);
/// Grace between the SIGTERM request and the SIGKILL escalation.
pub(super) const TERMINATION_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Who started the background compactor.
pub(super) enum CompactionOrigin {
    Manual,
    Auto,
}

impl CompactionOrigin {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why the compactor was asked to terminate early.
pub(super) enum TerminationReason {
    /// Explicit `/session compact cancel`.
    UserCancel,
    /// The total execution deadline elapsed.
    DeadlineExceeded,
}

#[derive(Debug, Clone, Copy)]
/// In-flight termination: SIGTERM was sent; SIGKILL escalates at `kill_at`.
pub(super) struct TerminationState {
    pub(super) reason: TerminationReason,
    pub(super) kill_at: Instant,
}

/// How a compactor is being launched, carrying exactly the data each mode
/// requires. Making this an enum keeps the illegal `Auto` without an expected
/// revision unrepresentable: an automatic run must always name the generation
/// and projection revision it is bound to.
pub(super) enum CompactionKind {
    /// Explicit `/session compact`: no revision binding, no suppression.
    Manual,
    /// Idle-boundary automatic run bound to the exact recommended context.
    Auto {
        generation: u64,
        projection_revision: u64,
    },
}

/// Per-revision suppression identity for automatic compaction.
///
/// Includes the canonical session ID so a failure on one session can never
/// suppress an identically-numbered `generation:revision` on a different
/// session (the runtime state can outlive a session selection change).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SuppressionMarker {
    pub(super) session_id: String,
    pub(super) generation: u64,
    pub(super) projection_revision: u64,
}

/// Parsed result of one compactor process.
pub(super) enum CompactionOutcome {
    Committed {
        tokens_before: u64,
        tokens_after: u64,
        after_source: String,
    },
    Failed {
        code: String,
        message: String,
    },
}

/// A running background compactor and its owned child handle.
pub(super) struct ActiveCompaction {
    pub(super) session_id: String,
    pub(super) workspace_scope: String,
    pub(super) started_at: Instant,
    /// Absolute wall-clock bound for this run; `poll` starts termination once
    /// it passes. Tests may shorten it directly.
    pub(super) deadline: Instant,
    pub(super) origin: CompactionOrigin,
    /// Per-revision suppression identity for automatic attempts (`None` for
    /// manual runs, which are never suppressed).
    pub(super) revision_marker: Option<SuppressionMarker>,
    /// Set once a SIGTERM was issued (cancel or deadline); drives the
    /// grace-then-SIGKILL escalation in `poll`. The first reason wins.
    pub(super) termination: Option<TerminationState>,
    /// Bounded tail of the child's stderr, drained continuously off-thread.
    pub(super) stderr_tail: StderrTail,
    // Single synchronized owner of the child handle. Signals are only ever
    // sent while the un-reaped child is held, so the PID stays valid.
    pub(super) child: Arc<Mutex<Option<Child>>>,
    pub(super) receiver: mpsc::Receiver<CompactionOutcome>,
}

impl ActiveCompaction {
    /// Whether the user explicitly requested cancellation.
    pub(super) fn cancel_requested(&self) -> bool {
        matches!(
            self.termination,
            Some(TerminationState {
                reason: TerminationReason::UserCancel,
                ..
            })
        )
    }

    /// Starts (or keeps) the termination state machine: SIGTERM to the
    /// process group now, SIGKILL escalation after [`TERMINATION_GRACE`].
    ///
    /// Idempotent — a second request (e.g. deadline elapsing after a user
    /// cancel) keeps the original reason and grace deadline.
    pub(super) fn request_termination(&mut self, reason: TerminationReason) {
        if self.termination.is_some() {
            return;
        }
        self.signal_terminate();
        self.termination = Some(TerminationState {
            reason,
            kill_at: Instant::now() + TERMINATION_GRACE,
        });
    }

    /// Signals the compactor process group while the child is still owned.
    pub(super) fn signal_terminate(&self) {
        if let Some(child) = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            terminate_process_group(child.id());
        }
    }

    /// Terminates and reaps the child through the single owning handle.
    ///
    /// Idempotent: taking the child out of the mutex means a later `Drop`
    /// (or a second `poll`) finds nothing to reap.
    pub(super) fn terminate_and_reap(&self) {
        if let Some(mut child) = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            terminate_and_reap_process(&mut child);
        }
    }
}

impl Drop for ActiveCompaction {
    fn drop(&mut self) {
        // Shell exit and state teardown must terminate and reap the
        // compactor. Taking the child from the mutex makes this idempotent
        // with `poll`, which reaps on the normal completion path.
        self.terminate_and_reap();
    }
}

/// Spawns the detached compactor in its own process group.
///
/// An automatic attempt is bound to the exact context the recommendation
/// observed: the `--auto-compact` flag distinguishes it from a manual run in
/// reporting, and the expected generation/revision let cosh-core fail closed
/// before spending a provider call if the session moved. Because `kind`
/// carries the generation/revision inline for [`CompactionKind::Auto`], an
/// automatic launch can never be constructed without them.
pub(super) fn spawn_compactor(
    program: &str,
    workspace: &str,
    session_id: &str,
    kind: CompactionKind,
) -> std::io::Result<ActiveCompaction> {
    let mut command = Command::new(program);
    command.args([
        "--headless",
        "--workspace",
        workspace,
        "--resume",
        session_id,
        "--compact",
    ]);
    let (origin, revision_marker) = match kind {
        CompactionKind::Manual => (CompactionOrigin::Manual, None),
        CompactionKind::Auto {
            generation,
            projection_revision,
        } => {
            let generation_arg = generation.to_string();
            let revision_arg = projection_revision.to_string();
            command.args([
                "--auto-compact",
                "--expect-generation",
                &generation_arg,
                "--expect-revision",
                &revision_arg,
            ]);
            (
                CompactionOrigin::Auto,
                Some(SuppressionMarker {
                    session_id: session_id.to_string(),
                    generation,
                    projection_revision,
                }),
            )
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take();
    // Stderr is drained continuously into a bounded tail: the pipe can never
    // back-pressure the child, retention is capped, and an abnormal exit
    // still leaves a diagnostic trace for the failure panel.
    let stderr_tail = StderrTail::new(MAX_COMPACTOR_STDERR_BYTES);
    if let Some(stderr) = child.stderr.take() {
        stderr_tail.drain_in_background(stderr);
    }
    let (sender, receiver) = mpsc::channel();
    // The reader thread only owns stdout; the child handle stays in one
    // synchronized owner so reaping and signalling can never race a PID.
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(stdout) = stdout {
            let _ = stdout
                .take(MAX_COMPACTOR_OUTPUT_BYTES)
                .read_to_string(&mut text);
        }
        let _ = sender.send(parse_compactor_output(&text));
    });
    let now = Instant::now();
    Ok(ActiveCompaction {
        session_id: session_id.to_string(),
        workspace_scope: workspace.to_string(),
        started_at: now,
        deadline: now + COMPACTOR_DEADLINE,
        origin,
        revision_marker,
        termination: None,
        stderr_tail,
        child: Arc::new(Mutex::new(Some(child))),
        receiver,
    })
}

/// Protocol values a compaction report may name as its measurement source.
///
/// Deserialization is restricted to the known protocol labels so a
/// compromised or corrupted compactor cannot smuggle arbitrary text into the
/// completion panel through this field; any other value fails the envelope.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportedTokenSource {
    ProviderReported,
    Estimated,
}

impl ReportedTokenSource {
    fn label(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::Estimated => "estimated",
        }
    }
}

/// A token measurement as serialized by cosh-core's compaction report.
#[derive(serde::Deserialize)]
struct TokenMeasurement {
    value: u64,
    source: ReportedTokenSource,
}

/// The `data` payload of a successful compaction envelope. Deserialization
/// fails (rather than defaulting) when any required field is absent or the
/// wrong type, so an incomplete envelope can never be read as `0 -> 0`.
#[derive(serde::Deserialize)]
struct CompactSuccessData {
    tokens_before: TokenMeasurement,
    tokens_after: TokenMeasurement,
}

/// Parses the single JSON envelope emitted by `cosh-core --compact`.
///
/// A `{"ok":true}` envelope is only accepted as a committed compaction when
/// it carries fully-typed `tokens_before.value`, `tokens_after.value`, and a
/// known protocol `tokens_after.source`. A truthful-looking but incomplete
/// success envelope is reported as a protocol failure, never as a zero-token
/// success, so the user is not told a compaction happened when the result is
/// unknown. Error fields are child-controlled and sanitized here: the code is
/// forced into a bounded snake_case shape and the message is redacted and
/// bounded again before rendering.
pub(super) fn parse_compactor_output(text: &str) -> CompactionOutcome {
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(ok) = value.get("ok").and_then(serde_json::Value::as_bool) else {
            continue;
        };
        if ok {
            return parse_success_envelope(value.get("data"));
        }
        let field = |name: &str| {
            value
                .get("error")
                .and_then(|error| error.get(name))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        return CompactionOutcome::Failed {
            code: sanitize_error_code(&field("code")),
            message: field("message"),
        };
    }
    CompactionOutcome::Failed {
        code: "transport".to_string(),
        message: "compactor produced no parseable result".to_string(),
    }
}

/// Maximum characters accepted for a child-reported error code.
const MAX_ERROR_CODE_CHARS: usize = 32;

/// Forces a child-reported error code into a bounded `snake_case` shape.
///
/// The code is rendered verbatim inside the failure panel, so it must never
/// carry control sequences, spacing tricks, or unbounded length. Anything
/// that does not survive the character filter is reported as `protocol`.
fn sanitize_error_code(code: &str) -> String {
    let sanitized: String = code
        .chars()
        .filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '_')
        .take(MAX_ERROR_CODE_CHARS)
        .collect();
    if sanitized.is_empty() {
        "protocol".to_string()
    } else {
        sanitized
    }
}

/// Validates a success envelope's `data` payload, failing closed on anything
/// incomplete or malformed — including a measurement source outside the
/// known protocol values.
fn parse_success_envelope(data: Option<&serde_json::Value>) -> CompactionOutcome {
    let protocol_failure = || CompactionOutcome::Failed {
        code: "protocol".to_string(),
        message: "compactor reported success with an incomplete result envelope".to_string(),
    };
    let Some(data) = data else {
        return protocol_failure();
    };
    let Ok(parsed) = serde_json::from_value::<CompactSuccessData>(data.clone()) else {
        return protocol_failure();
    };
    CompactionOutcome::Committed {
        tokens_before: parsed.tokens_before.value,
        tokens_after: parsed.tokens_after.value,
        after_source: parsed.tokens_after.source.label().to_string(),
    }
}

/// Sanitizes and truncates untrusted subprocess text for panel display.
///
/// Every child-controlled diagnostic (structured error message, stderr tail,
/// spawn error) funnels through here, in this exact order:
/// 1. canonicalize terminal sequences: whole CSI/OSC/DCS/SOS/PM/APC bodies
///    (ESC and C1 forms) are consumed — no `[31m`-style residue, no string
///    payload — and invisible Unicode format characters (zero-width, bidi
///    controls, variation selectors, tags) are dropped;
/// 2. drop every remaining control character;
/// 3. redact secrets on the canonicalized text — running redaction any
///    earlier lets invisible characters split a sensitive key
///    (`api_\0key=...`, `api_\u{200B}key=...`) so the patterns never see the
///    canonical form while the terminal still shows a readable secret;
/// 4. defensively filter control characters again (redaction must never
///    reintroduce any);
/// 5. truncate on a char boundary.
pub(super) fn bounded(value: &str) -> String {
    let cleaned = crate::evidence::clean_terminal_control_sequences(value);
    let canonical: String = cleaned.chars().filter(|ch| !ch.is_control()).collect();
    let redacted = crate::evidence::redact_sensitive_text(&canonical).0;
    let sanitized: String = redacted.chars().filter(|ch| !ch.is_control()).collect();
    if sanitized.chars().count() <= MAX_REPORTED_ERROR_CHARS {
        return sanitized;
    }
    let mut truncated: String = sanitized.chars().take(MAX_REPORTED_ERROR_CHARS).collect();
    truncated.push('…');
    truncated
}
