// Owner: shell_host. Routing lives in osc/routing.rs; the pending-handoff
// claim lives in osc/handoff_claim.rs; alt-screen tracking in osc/alt_screen.rs.
// CurrentCommand state and display-window helpers are owned by osc/command.rs.
mod alt_screen;
mod command;
mod event_store;
mod handoff_claim;
mod handoff_echo;
mod marker_sequence;
mod prompt_epoch;
mod routing;
mod slash_guard_echo;
mod transcript_store;

use alt_screen::AltScreenTracker;
use command::{is_shell_exit_command, CurrentCommand};
pub(crate) use command::{VisibleTailTracker, VISIBLE_TAIL_MAX_CHARS};
use event_store::EventStore;
use handoff_claim::{
    claim_pending_command_origin, pending_origin_for_request, PendingCommandOrigin,
};
use handoff_echo::PendingHandoffEcho;
use marker_sequence::resume_abandoned_prefix;
use marker_sequence::{find_bytes, osc_prefix_suffix_len, HistoryFileTracker, Marker};
use slash_guard_echo::PendingSlashGuardEcho;

use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::model::{ShellEnvironmentObserver, ShellHistoryFileObserver};
use super::transcript::Transcript;
use crate::types::{
    CommandOrigin, ShellCaptureLifecycle, ShellCaptureMetadata, ShellCommandAuditIdentity,
    ShellEnvironmentSnapshot, ShellEvent, ShellEventKind, ShellHandoffRequest,
};

#[cfg(test)]
pub(super) use super::osc_output::{
    capped_output_ref_bytes, write_output_ref, write_output_ref_with_session_cap,
};
pub(super) use super::osc_output::{OutputRefCapture, OutputRefCaptureStatus};

const OSC_PREFIX: &[u8] = b"\x1b]1337;COSH;";
const BRACKETED_PASTE_ENABLE: &[u8] = b"\x1b[?2004h";
const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";
const STYLE_RESET: &[u8] = b"\x1b[0m";
const REVERSE_OFF: &[u8] = b"\x1b[27m";
const UNDERLINE_OFF: &[u8] = b"\x1b[24m";
const ERASE_TO_END_OF_SCREEN: &[u8] = b"\x1b[J";
const ERASE_TO_END_OF_LINE: &[u8] = b"\x1b[K";
const BEL: u8 = b'\x07';
const OSC_CANDIDATE_MAX_BYTES: usize = 64 * 1024;
const SHELL_PATH_MAX_BYTES: usize = 8 * 1024;

#[cfg(test)]
thread_local! {
    static OSC_CANDIDATE_SCAN_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Why a display cut was recorded, so consumers can tell an intercepted
/// input (shell has not painted a prompt for it yet) apart from a real
/// prompt boundary (precmd/shell-ready).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DisplayCutKind {
    Intercept,
    PromptBoundary,
}

#[derive(Debug)]
pub(super) struct OscParser {
    pub(super) session_id: String,
    output_ref_dir: PathBuf,
    pub(super) events: EventStore,
    pub(super) clean: Transcript,
    pub(super) display: Transcript,
    marker_token: Option<String>,
    pending: Vec<u8>,
    /// Leading pending bytes inherited from a discarded private candidate;
    /// tentative continuation bytes after this boundary remain terminal output.
    abandoned_prefix_len: usize,
    pending_clean_control: Vec<u8>,
    current: Option<CurrentCommand>,
    command_seq: usize,
    intervention_cuts: Vec<usize>,
    intervention_display_cuts: Vec<(usize, DisplayCutKind)>,
    last_prompt_display_start: Option<usize>,
    last_prompt_display: Vec<u8>,
    capture_prompt_display: bool,
    prompt_ready_display_start: Option<usize>,
    prompt_presentation_display_starts: Vec<super::prompt_presentation::PromptDisplayStart>,
    prompt_epoch_exchange: Option<crate::raw_input::PromptEpochExchange>,
    prompt_epoch: Option<u64>,
    /// #1932: the soft-newline upgrade submitted a synthetic empty line so
    /// bash repaints PS1; its visually blank accept echo is dropped at the
    /// matching prompt boundary instead of surfacing as a blank line.
    synthetic_prompt_repaint_armed: bool,
    pub(super) captured_output_ref_bytes: usize,
    pending_command_origin: Option<PendingCommandOrigin>,
    /// Raised when a command-less prompt boundary expired an unclaimed
    /// pending handoff (#2142 R4); the relay consumes it and removes the
    /// staged request/token sidecars the shell never claimed.
    expired_handoff_staging: bool,
    pending_handoff_echo: Option<PendingHandoffEcho>,
    pending_slash_guard_echo: Option<PendingSlashGuardEcho>,
    pub(super) shell_environment_snapshot: Option<ShellEnvironmentSnapshot>,
    environment_observer: Option<ShellEnvironmentObserver>,
    history_file_tracker: HistoryFileTracker,
    /// #1721 D16: shared "bash sits at PS1" gate consumed by the raw input
    /// relay; prompt_ready raises it, preexec lowers it.
    main_prompt_gate: crate::raw_input::MainPromptGate,
    /// Rust-owned controls read in the same batch as preceding shell lines.
    /// Each counter reaches zero only after those submissions reach PS1.
    pending_prompt_intercepts: Vec<(String, String, usize)>,
    /// A preexec or routing marker proves the next prompt boundary belongs to
    /// a submitted non-empty line rather than startup prompt initialization.
    submission_boundary_observed: bool,
    assistance_control: Option<crate::input::AssistanceControl>,
    /// Trusted primary-prompt cwd shared with the submit-time input router.
    shell_prompt_cwd: crate::input::ShellPromptCwd,
    /// Zsh-proven slash names shared with the submit-time input router.
    shell_path_command_names: crate::input::ShellPathCommandNames,
    /// Collapses consecutive PTY input writes into one prompt-cwd
    /// invalidation barrier; a fresh command-less prompt report
    /// (`ShellReady`) re-arms it.
    pty_input_barrier_pushed: bool,
    /// #2196 R7: bounded last-visible-line tracker for the active command,
    /// fed incrementally per PTY chunk; owned by osc/command.rs.
    visible_tail: VisibleTailTracker,
    /// #2025: alternate-screen tracking, owned by osc/alt_screen.rs.
    alt_screen: AltScreenTracker,
}

impl OscParser {
    /// Shares the main-prompt gate with the raw input relay (#1721 D16).
    pub(crate) fn set_main_prompt_gate(&mut self, gate: crate::raw_input::MainPromptGate) {
        self.main_prompt_gate = gate;
    }

    pub(crate) fn set_assistance_control(&mut self, control: crate::input::AssistanceControl) {
        self.assistance_control = Some(control);
    }

    pub(crate) fn set_prompt_cwd(&mut self, cwd: crate::input::ShellPromptCwd) {
        self.shell_prompt_cwd = cwd;
    }

    pub(crate) fn set_shell_path_command_names(
        &mut self,
        names: crate::input::ShellPathCommandNames,
    ) {
        self.shell_path_command_names = names;
    }

    pub(super) fn with_environment_observer(mut self, observer: ShellEnvironmentObserver) -> Self {
        self.environment_observer = Some(observer);
        self
    }

    pub(super) fn with_history_file_observer(mut self, observer: ShellHistoryFileObserver) -> Self {
        self.history_file_tracker.set_observer(observer);
        self
    }

    pub(super) fn register_pending_handoff_origin(&mut self, request: &ShellHandoffRequest) {
        self.pending_command_origin = Some(pending_origin_for_request(request));
        // Fresh staging supersedes any stale expiry signal.
        self.expired_handoff_staging = false;
    }

    pub(super) fn clear_pending_handoff_origin(&mut self) {
        self.pending_command_origin = None;
    }

    /// Consumes the "an unclaimed handoff expired at a prompt boundary" flag
    /// (#2142 R4); the relay clears the staged sidecar files in response.
    pub(super) fn take_expired_handoff_staging(&mut self) -> bool {
        std::mem::take(&mut self.expired_handoff_staging)
    }

    pub(super) fn feed(&mut self, data: &[u8]) -> io::Result<()> {
        if self.marker_token.is_none() {
            return self.append_passthrough(data);
        }
        let Some(data) =
            resume_abandoned_prefix(&mut self.pending, &mut self.abandoned_prefix_len, data)
        else {
            return Ok(());
        };
        let mut scan_from = self
            .pending
            .len()
            .saturating_sub(OSC_PREFIX.len().saturating_sub(1));
        self.pending.extend_from_slice(data);
        loop {
            let Some(start) = find_bytes(&self.pending, OSC_PREFIX) else {
                let keep = osc_prefix_suffix_len(&self.pending);
                let flush_len = self.pending.len().saturating_sub(keep);
                if flush_len > 0 {
                    let passthrough = self.pending[..flush_len].to_vec();
                    self.append_passthrough(&passthrough)?;
                    self.pending.drain(..flush_len);
                }
                return Ok(());
            };

            if start > 0 {
                let passthrough = self.pending[..start].to_vec();
                self.append_passthrough(&passthrough)?;
                self.pending.drain(..start);
                scan_from = scan_from.saturating_sub(start);
            }

            let payload_start = OSC_PREFIX.len();
            scan_from = scan_from.max(payload_start).min(self.pending.len());
            let candidate = &self.pending[scan_from..];
            let terminator_offset = candidate.iter().position(|byte| *byte == BEL);
            #[cfg(test)]
            {
                let scanned = terminator_offset.map_or(candidate.len(), |idx| idx + 1);
                OSC_CANDIDATE_SCAN_BYTES
                    .set(OSC_CANDIDATE_SCAN_BYTES.get().saturating_add(scanned));
            }
            let terminator = terminator_offset.map(|idx| idx + scan_from);
            let restart_before = terminator.unwrap_or(self.pending.len());
            let restart_candidate = &self.pending[scan_from..restart_before];
            #[cfg(test)]
            {
                OSC_CANDIDATE_SCAN_BYTES.set(
                    OSC_CANDIDATE_SCAN_BYTES
                        .get()
                        .saturating_add(restart_candidate.len()),
                );
            }
            let restart = restart_candidate
                .windows(OSC_PREFIX.len())
                .rposition(|window| window == OSC_PREFIX)
                .map(|idx| idx + scan_from);
            if let Some(restart) = restart {
                self.pending.drain(..restart);
                scan_from = payload_start;
                continue;
            }

            let Some(end) = terminator else {
                if self.pending.len() > OSC_CANDIDATE_MAX_BYTES {
                    let keep = osc_prefix_suffix_len(&self.pending);
                    let split_at = self.pending.len() - keep;
                    let suffix = self.pending.split_off(split_at);
                    self.pending = suffix;
                    self.abandoned_prefix_len = self.pending.len();
                }
                return Ok(());
            };
            if end + 1 > OSC_CANDIDATE_MAX_BYTES {
                self.pending.drain(..=end);
                scan_from = 0;
                continue;
            }

            let payload = self.pending[payload_start..end].to_vec();
            self.pending.drain(..=end);
            scan_from = 0;
            match serde_json::from_slice::<Marker>(&payload) {
                Ok(marker) => self.handle_marker(marker)?,
                Err(err) => self.events.push(ShellEvent {
                    kind: ShellEventKind::ComponentFailed,
                    session_id: self.session_id.clone(),
                    command_id: None,
                    command: None,
                    cwd: None,
                    end_cwd: None,
                    exit_code: None,
                    started_at_ms: Some(now_ms()),
                    ended_at_ms: None,
                    duration_ms: None,
                    terminal_output_ref: None,
                    terminal_output_bytes: None,
                    input: None,
                    component: Some("osc_parser".to_string()),
                    message: Some(format!("marker parse failed: {err}")),
                    command_origin: None,
                    shell_environment_generation: None,
                    audit_identity: None,
                    routing: None,
                    capture: None,
                }),
            }
        }
    }

    #[cfg(test)]
    pub(super) fn reset_osc_candidate_scan_bytes() {
        OSC_CANDIDATE_SCAN_BYTES.set(0);
    }

    #[cfg(test)]
    pub(super) fn osc_candidate_scan_bytes() -> usize {
        OSC_CANDIDATE_SCAN_BYTES.get()
    }

    fn handle_marker(&mut self, mut marker: Marker) -> io::Result<()> {
        if marker.token.as_deref() != self.marker_token.as_deref() {
            return Ok(());
        }

        if marker
            .session_id
            .as_deref()
            .is_some_and(|session_id| session_id != self.session_id)
        {
            return Ok(());
        }

        // A trusted protocol boundary ends the one-shot submission window.
        // Any incomplete candidate is ordinary terminal output and fails open.
        self.flush_pending_handoff_echo()?;

        let compact_prompt_marker = marker.normalize_compact_prompt();

        if self.handle_slash_guard_marker(&marker)? {
            return Ok(());
        }

        if marker.has_trusted_history_context(&self.session_id, compact_prompt_marker) {
            self.history_file_tracker
                .observe(marker.history_file.as_deref());
        }
        if marker.event == "history_file" {
            return Ok(());
        }

        let environment_generation = self.observe_shell_environment(&marker);
        let session_id = marker
            .session_id
            .clone()
            .unwrap_or_else(|| self.session_id.clone());
        let timestamp = marker.timestamp_ms.unwrap_or_else(now_ms);
        let prompt_ready_with_precmd = marker.prompt_ready.unwrap_or(false);
        let shell_path_names = marker.shell_path_names.clone();
        let shell_path_suffixes = marker.shell_path_suffixes.clone();

        if matches!(marker.event.as_str(), "intercept" | "top_level_missing") {
            self.submission_boundary_observed = true;
            self.handle_routing_marker(marker, session_id, timestamp);
            return Ok(());
        }

        match marker.event.as_str() {
            "prompt_ready" => {
                self.mark_prompt_ready(marker.physical_cwd, shell_path_names, shell_path_suffixes);
            }
            "preexec" => {
                self.submission_boundary_observed = true;
                if !self.display.is_full() {
                    self.capture_prompt_display = false;
                }
                self.main_prompt_gate.set_at_prompt(false);
                if let Some(control) = &self.assistance_control {
                    control.set_at_prompt(false);
                }
                // #2196 R7: the visible-tail window starts at the command
                // boundary, so earlier output can never resurface as the
                // prompt tail.
                self.visible_tail.reset();
                let command = marker.command.unwrap_or_default();
                self.command_seq += 1;
                let command_id = format!("cmd-{}", self.command_seq);
                let cwd = marker.cwd.unwrap_or_default();
                let (origin, audit_identity) = claim_pending_command_origin(
                    &mut self.pending_command_origin,
                    &command,
                    marker.handoff.as_deref(),
                );
                self.current = Some(CurrentCommand {
                    id: command_id.clone(),
                    command: command.clone(),
                    cwd: cwd.clone(),
                    origin,
                    audit_identity: audit_identity.clone(),
                    started_at_ms: timestamp,
                    output_start: self.clean.position(),
                    attempt_generation: marker.generation,
                    shell_environment_generation: marker
                        .path_trusted
                        .unwrap_or(false)
                        .then_some(environment_generation)
                        .flatten(),
                });
                let mut event = ShellEvent::command_started_with_origin(
                    session_id, command_id, command, cwd, timestamp, origin,
                );
                event.shell_environment_generation = self
                    .current
                    .as_ref()
                    .and_then(|current| current.shell_environment_generation);
                event.audit_identity = audit_identity;
                self.events.push(event);
            }
            "precmd" => {
                self.prompt_ready_display_start = None;
                let Some(current) = self.current.take() else {
                    let prompt_cwd = marker.physical_cwd.clone();
                    self.intervention_cuts.push(self.clean.position());
                    self.intervention_display_cuts
                        .push((self.display.position(), DisplayCutKind::PromptBoundary));
                    self.start_prompt_display_capture();
                    // A fresh command-less prompt report re-arms the
                    // PTY-input invalidation barrier: the cwd carried
                    // here supersedes any earlier barrier.
                    self.pty_input_barrier_pushed = false;
                    // A command-less prompt boundary is exactly where the
                    // runtime closes an unclaimed handoff as untracked
                    // (#2142 review R4). Expire the pending claim slot and
                    // ask the relay to remove the staged sidecars, so a later
                    // same-text user command can neither adopt the closed
                    // handoff's identity nor read its plaintext command.
                    if self.pending_command_origin.take().is_some() {
                        self.expired_handoff_staging = true;
                    }
                    self.events.push(ShellEvent {
                        kind: ShellEventKind::ShellReady,
                        session_id,
                        command_id: None,
                        command: None,
                        cwd: marker.cwd,
                        end_cwd: None,
                        exit_code: None,
                        started_at_ms: Some(timestamp),
                        ended_at_ms: None,
                        duration_ms: None,
                        terminal_output_ref: None,
                        terminal_output_bytes: None,
                        input: None,
                        component: None,
                        message: None,
                        command_origin: None,
                        shell_environment_generation: None,
                        audit_identity: None,
                        routing: None,
                        capture: None,
                    });
                    if prompt_ready_with_precmd {
                        self.mark_prompt_ready(prompt_cwd, shell_path_names, shell_path_suffixes);
                    }
                    return Ok(());
                };

                let status = if is_shell_exit_command(&current.command) {
                    0
                } else {
                    // Missing status = truncation/forgery/drift (always emitted) → -1, not success (#2413/#2105).
                    marker.status.unwrap_or(-1)
                };
                let output_end = self.clean.position();
                let output_ref =
                    self.capture_command_output_ref(&current.id, current.output_start, output_end)?;
                self.intervention_cuts.push(output_end);
                self.intervention_display_cuts
                    .push((self.display.position(), DisplayCutKind::PromptBoundary));
                self.start_prompt_display_capture();
                let kind = if status == 0 {
                    ShellEventKind::CommandCompleted
                } else {
                    ShellEventKind::CommandFailed
                };

                let mut event = command_finished_event(
                    kind,
                    session_id,
                    current.id,
                    status,
                    timestamp,
                    &output_ref,
                );
                event.command = Some(current.command);
                event.cwd = Some(current.cwd.clone());
                let prompt_cwd = marker.physical_cwd.clone();
                event.end_cwd = marker.cwd.or(Some(current.cwd));
                event.duration_ms = Some(timestamp.saturating_sub(current.started_at_ms));
                event.terminal_output_bytes =
                    Some(output_end.saturating_sub(current.output_start) as u64);
                event.command_origin = Some(current.origin);
                event.audit_identity = current.audit_identity;
                event.shell_environment_generation = current.shell_environment_generation;
                self.events.push(event);
                if prompt_ready_with_precmd {
                    self.mark_prompt_ready(prompt_cwd, shell_path_names, shell_path_suffixes);
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn mark_prompt_ready(
        &mut self,
        prompt_cwd: Option<String>,
        shell_path_names: Option<Vec<String>>,
        shell_path_suffixes: Option<Vec<String>>,
    ) {
        self.shell_prompt_cwd.set(prompt_cwd.clone());
        self.shell_path_command_names
            .set(shell_path_names, shell_path_suffixes);
        self.open_prompt_epoch();
        if !self.display.is_full() {
            self.start_prompt_display_capture();
        }
        self.prompt_ready_display_start = Some(self.display.position());
        self.prompt_presentation_display_starts.push(
            super::prompt_presentation::PromptDisplayStart {
                position: self.display.position(),
                publish_status: true,
            },
        );
        self.main_prompt_gate.set_at_prompt(true);
        if let Some(control) = &self.assistance_control {
            control.set_at_prompt(true);
        }
        let acknowledge_submission = std::mem::take(&mut self.submission_boundary_observed);
        let pending = std::mem::take(&mut self.pending_prompt_intercepts);
        let session_id = self.session_id.clone();
        for (input, reason, mut remaining) in pending {
            if acknowledge_submission {
                remaining = remaining.saturating_sub(1);
            }
            if remaining == 0 {
                self.push_intercept_event(&session_id, input, prompt_cwd.clone(), &reason);
            } else {
                self.pending_prompt_intercepts
                    .push((input, reason, remaining));
            }
        }
    }

    fn observe_shell_environment(&mut self, marker: &Marker) -> Option<u64> {
        if !matches!(marker.event.as_str(), "precmd" | "preexec") {
            return None;
        }
        if marker.session_id.as_deref() != Some(self.session_id.as_str()) {
            return None;
        }
        let path = marker.path.as_deref()?;
        if path.len() > SHELL_PATH_MAX_BYTES || path.chars().any(char::is_control) {
            return None;
        }
        let normalized = normalize_shell_path(path);
        let marker_sequence = self
            .shell_environment_snapshot
            .as_ref()
            .map_or(Some(1), |snapshot| snapshot.marker_sequence.checked_add(1))?;
        let generation = self
            .shell_environment_snapshot
            .as_ref()
            .map_or(Some(1), |snapshot| {
                if snapshot.path == normalized {
                    Some(snapshot.generation)
                } else {
                    snapshot.generation.checked_add(1)
                }
            })?;
        let snapshot = ShellEnvironmentSnapshot {
            session_id: self.session_id.clone(),
            marker_sequence,
            generation,
            path: normalized,
            // Precmd markers carry the CNF ownership; preexec markers do not,
            // so retain the last reported value instead of clearing it.
            cnf_handler: marker.cnf.clone().or_else(|| {
                self.shell_environment_snapshot
                    .as_ref()?
                    .cnf_handler
                    .clone()
            }),
        };
        self.shell_environment_snapshot = Some(snapshot.clone());
        if let Some(observer) = &self.environment_observer {
            observer.observe(snapshot);
        }
        Some(generation)
    }

    fn append_passthrough(&mut self, data: &[u8]) -> io::Result<()> {
        let data = PendingSlashGuardEcho::filter(&mut self.pending_slash_guard_echo, data);
        let data = PendingHandoffEcho::filter(&mut self.pending_handoff_echo, &data);
        if data.is_empty() {
            return Ok(());
        }
        self.alt_screen.observe(&data);
        self.visible_tail.feed(&data);
        self.display.append(&data)?;
        self.append_prompt_display_tail(&data);
        self.append_clean(&data)
    }

    /// #2025: whether the foreground application currently owns the
    /// alternate screen (fullscreen TUI classification input).
    pub(crate) fn alt_screen_active(&self) -> bool {
        self.alt_screen.active()
    }

    fn append_clean(&mut self, data: &[u8]) -> io::Result<()> {
        let mut bytes = Vec::new();
        if !self.pending_clean_control.is_empty() {
            bytes.append(&mut self.pending_clean_control);
        }
        bytes.extend_from_slice(data);

        let mut run = Vec::with_capacity(bytes.len());
        let mut idx = 0;
        while idx < bytes.len() {
            let rest = &bytes[idx..];
            if let Some(control_len) = known_clean_control_len(rest) {
                self.clean.append(&run)?;
                run.clear();
                idx += control_len;
                continue;
            }
            if is_known_clean_control_prefix(rest) {
                self.clean.append(&run)?;
                self.pending_clean_control.extend_from_slice(rest);
                return Ok(());
            }
            if bytes[idx] == b'\x08' {
                self.clean.append(&run)?;
                run.clear();
                self.clean.pop_last_utf8_char()?;
            } else {
                run.push(bytes[idx]);
            }
            idx += 1;
        }
        self.clean.append(&run)?;
        Ok(())
    }

    fn push_clean_byte(&mut self, byte: u8) -> io::Result<()> {
        if byte == b'\x08' {
            return self.clean.pop_last_utf8_char();
        }
        self.clean.append(&[byte])
    }

    fn flush_pending_clean_control(&mut self) -> io::Result<()> {
        let pending = std::mem::take(&mut self.pending_clean_control);
        for byte in pending {
            self.push_clean_byte(byte)?;
        }
        Ok(())
    }

    pub(super) fn finish_current_on_exit(&mut self, status: i32) -> io::Result<()> {
        let Some(current) = self.current.take() else {
            return Ok(());
        };

        let ended_at = now_ms();
        let output_end = self.clean.position();
        let output_ref =
            self.capture_command_output_ref(&current.id, current.output_start, output_end)?;
        let status = if is_shell_exit_command(&current.command) {
            0
        } else {
            status
        };
        let kind = if status == 0 {
            ShellEventKind::CommandCompleted
        } else {
            ShellEventKind::CommandFailed
        };
        let mut event = command_finished_event(
            kind,
            self.session_id.clone(),
            current.id,
            status,
            ended_at,
            &output_ref,
        );
        event.command = Some(current.command);
        event.cwd = Some(current.cwd.clone());
        event.end_cwd = Some(current.cwd);
        event.duration_ms = Some(ended_at.saturating_sub(current.started_at_ms));
        event.terminal_output_bytes = Some(output_end.saturating_sub(current.output_start) as u64);
        event.command_origin = Some(current.origin);
        event.audit_identity = current.audit_identity;
        event.shell_environment_generation = current.shell_environment_generation;
        self.events.push(event);
        Ok(())
    }

    pub(super) fn prompt_count(&self, prompt: &[u8]) -> usize {
        if prompt.is_empty() {
            return 0;
        }
        // Bounded mode uses this only for the isolated-shell startup gate;
        // the prompt that just painted is necessarily in the resident tail.
        self.clean
            .resident_slice()
            .windows(prompt.len())
            .filter(|window| *window == prompt)
            .count()
    }

    pub(super) fn precmd_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    ShellEventKind::CommandCompleted
                        | ShellEventKind::CommandFailed
                        | ShellEventKind::ShellReady
                )
            })
            .count()
    }

    pub(super) fn drain_intervention_display_cuts(&mut self) -> Vec<(usize, DisplayCutKind)> {
        std::mem::take(&mut self.intervention_display_cuts)
    }

    pub(super) fn drain_prompt_presentation_display_starts(
        &mut self,
    ) -> Vec<super::prompt_presentation::PromptDisplayStart> {
        std::mem::take(&mut self.prompt_presentation_display_starts)
    }

    /// Arms the one-shot blank-echo drop for the synthetic PS1 repaint
    /// submitted by the soft-newline upgrade (#1932).
    pub(super) fn arm_synthetic_prompt_repaint(&mut self) {
        self.synthetic_prompt_repaint_armed = true;
    }

    /// Consumes the one-shot arm at the matching prompt boundary.
    pub(super) fn take_synthetic_prompt_repaint(&mut self) -> bool {
        std::mem::take(&mut self.synthetic_prompt_repaint_armed)
    }

    pub(super) fn push_intercept_event(
        &mut self,
        session_id: &str,
        input: String,
        cwd: Option<String>,
        reason: &str,
    ) {
        self.push_intercept_event_with_routing(session_id, input, cwd, reason, None, false, false);
    }

    pub(super) fn push_intercept_event_at_prompt(
        &mut self,
        input: String,
        reason: &str,
        pending_submits: usize,
    ) {
        if pending_submits == 0 && self.main_prompt_gate.is_at_prompt() {
            let session_id = self.session_id.clone();
            self.push_intercept_event(&session_id, input, None, reason);
        } else {
            self.pending_prompt_intercepts.push((
                input,
                reason.to_string(),
                pending_submits.max(1),
            ));
        }
    }

    pub(super) fn push_control_event(&mut self, input: &str) {
        self.push_self_session_input_event(
            "control",
            "control input observed while relaying to bash",
            Some(input),
        );
    }

    /// Observe-only soft-newline shortcut signal on a passthrough path
    /// (#1721 T-c): the bytes were relayed to bash unchanged; the runtime
    /// may surface a one-time discoverability tip at the next prompt-ready.
    pub(super) fn push_soft_newline_shortcut_event(&mut self) {
        self.push_self_session_input_event(
            "soft_newline_shortcut",
            "soft-newline shortcut observed while relaying to bash",
            None,
        );
    }

    /// #1932 F5: a multi-line bracketed paste was relayed straight to bash;
    /// the runtime may attach a multi-line entry hint to a failure insight.
    pub(super) fn push_multiline_paste_event(&mut self) {
        self.push_self_session_input_event(
            "multiline_paste",
            "multi-line bracketed paste relayed to bash",
            None,
        );
    }

    /// #1721 D13: forwards prompt-draft card lifecycle events (open/changed/
    /// submit/cancel) to the runtime as structured JSON payloads.
    pub(super) fn push_prompt_draft_event(&mut self, action: &str, payload: Option<&str>) {
        self.push_self_session_input_event("prompt_draft", action, payload);
    }

    pub(super) fn push_shell_input_activity_event(&mut self, empty: bool) {
        self.push_self_session_input_event(
            "shell_input",
            if empty {
                "input empty"
            } else {
                "input editing"
            },
            None,
        );
    }

    /// User bytes were written to the shell's PTY: whatever the shell
    /// does with them (a custom `accept-line` binding cannot be told
    /// apart from editing keys in the byte stream), a previously
    /// reported prompt cwd stops being provably current until a fresh
    /// cwd-bearing marker arrives. Consecutive writes collapse into
    /// one barrier event; a new prompt report re-arms it.
    pub(super) fn push_shell_pty_input_event(&mut self) {
        if self.pty_input_barrier_pushed {
            return;
        }
        self.pty_input_barrier_pushed = true;
        self.push_self_session_input_event("shell_pty_input", "write", None);
    }

    pub(super) fn push_card_event(&mut self, action: &str, value: &str) {
        self.push_self_session_input_event("card", action, Some(value));
    }

    pub(super) fn push_capture_event(
        &mut self,
        lifecycle: ShellCaptureLifecycle,
        generation: u64,
        kind: Option<&str>,
        target_id: Option<&str>,
    ) {
        let message = match lifecycle {
            ShellCaptureLifecycle::Submitted => "capture_submitted",
            ShellCaptureLifecycle::Drained => "capture_drained",
            ShellCaptureLifecycle::Expired => "capture_expired",
            ShellCaptureLifecycle::Overflow => "capture_overflow",
            ShellCaptureLifecycle::InputRejected => "capture_input_rejected",
        };
        self.events.push(ShellEvent {
            kind: ShellEventKind::UserInputIntercepted,
            session_id: self.session_id.clone(),
            command_id: None,
            command: None,
            cwd: None,
            end_cwd: None,
            exit_code: None,
            started_at_ms: Some(now_ms()),
            ended_at_ms: None,
            duration_ms: None,
            terminal_output_ref: None,
            terminal_output_bytes: None,
            input: None,
            component: Some("card".to_string()),
            message: Some(message.to_string()),
            command_origin: None,
            shell_environment_generation: None,
            audit_identity: None,
            routing: None,
            capture: Some(ShellCaptureMetadata {
                kind: kind.map(str::to_string),
                target_id: target_id.map(str::to_string),
                generation,
                lifecycle,
            }),
        });
    }

    pub(super) fn push_secret_card_event(&mut self, action: &str, value: &str) {
        self.push_self_session_input_event("card_secret", action, Some(value));
    }

    pub(super) fn push_prompt_ghost_event(&mut self, action: &str, suggestion_id: Option<&str>) {
        let component = suggestion_id
            .map(|id| format!("prompt_ghost:{id}"))
            .unwrap_or_else(|| "prompt_ghost".to_string());
        self.push_self_session_input_event(&component, action, None);
    }

    fn push_self_session_input_event(
        &mut self,
        component: &str,
        message: &str,
        input: Option<&str>,
    ) {
        self.events.push(ShellEvent {
            kind: ShellEventKind::UserInputIntercepted,
            session_id: self.session_id.clone(),
            command_id: None,
            command: None,
            cwd: None,
            end_cwd: None,
            exit_code: None,
            started_at_ms: Some(now_ms()),
            ended_at_ms: None,
            duration_ms: None,
            terminal_output_ref: None,
            terminal_output_bytes: None,
            input: input.map(str::to_string),
            component: Some(component.to_string()),
            message: Some(message.to_string()),
            command_origin: None,
            shell_environment_generation: None,
            audit_identity: None,
            routing: None,
            capture: None,
        });
    }
}

fn command_finished_event(
    kind: ShellEventKind,
    session_id: String,
    command_id: String,
    exit_code: i32,
    ended_at_ms: u64,
    output_ref: &OutputRefCapture,
) -> ShellEvent {
    match &output_ref.path {
        Some(path) => ShellEvent::command_finished(
            kind,
            session_id,
            command_id,
            exit_code,
            ended_at_ms,
            path.display().to_string(),
        ),
        None => ShellEvent {
            kind,
            session_id,
            command_id: Some(command_id),
            command: None,
            cwd: None,
            end_cwd: None,
            exit_code: Some(exit_code),
            started_at_ms: None,
            ended_at_ms: Some(ended_at_ms),
            duration_ms: None,
            terminal_output_ref: None,
            terminal_output_bytes: Some(0),
            input: None,
            component: Some("output_capture".to_string()),
            message: Some(match output_ref.status {
                OutputRefCaptureStatus::Captured => "output_capture_status: captured".to_string(),
                OutputRefCaptureStatus::SessionCapReached => {
                    "output_capture_status: unavailable; reason: session_output_cap_reached"
                        .to_string()
                }
            }),
            command_origin: None,
            shell_environment_generation: None,
            audit_identity: None,
            routing: None,
            capture: None,
        },
    }
}

fn normalize_shell_path(path: &str) -> String {
    let mut seen = HashSet::new();
    path.split(':')
        .filter_map(normalize_absolute_path)
        .filter(|entry| seen.insert(entry.clone()))
        .collect::<Vec<_>>()
        .join(":")
}

fn normalize_absolute_path(value: &str) -> Option<String> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => normalized.push(".."),
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) => return None,
        }
    }
    Some(normalized.to_string_lossy().into_owned())
}

fn known_clean_control_len(bytes: &[u8]) -> Option<usize> {
    [
        BRACKETED_PASTE_ENABLE,
        BRACKETED_PASTE_DISABLE,
        STYLE_RESET,
        REVERSE_OFF,
        UNDERLINE_OFF,
        ERASE_TO_END_OF_SCREEN,
        ERASE_TO_END_OF_LINE,
    ]
    .into_iter()
    .find(|control| bytes.starts_with(control))
    .map(|control| control.len())
}

fn is_known_clean_control_prefix(bytes: &[u8]) -> bool {
    [
        BRACKETED_PASTE_ENABLE,
        BRACKETED_PASTE_DISABLE,
        STYLE_RESET,
        REVERSE_OFF,
        UNDERLINE_OFF,
        ERASE_TO_END_OF_SCREEN,
        ERASE_TO_END_OF_LINE,
    ]
    .into_iter()
    .any(|control| control.starts_with(bytes))
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
