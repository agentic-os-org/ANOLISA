//! Relay thread state shared across the spawn submodules (#1721 layout
//! split): the per-relay bookkeeping struct and its borrow-splitting helper.

use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::input::InputClassifier;

use super::super::capture_bridge::consume_captured_input;
use super::super::card_capture::CardInputState;
use super::super::event_parser::{CandidateLineBuffer, NativeLineState};
use super::super::event_sender::RawInputEventSink;
use super::super::generation::{LineSubmitCounter, UserPtyInputGeneration};
use super::super::mode::current_raw_input_mode;
use super::super::mode::RawInputMode;
use super::super::relay::{
    replay_deferred_zsh_tab_typeahead, ExplicitExitTracker, InputRelayContext,
};
use super::super::MainPromptGate;
use super::action::PendingDelayEscape;
use super::assistance::PendingAssistanceEscape;
use super::capture::{drain_capture_submission, CaptureOwnedInput};
use super::prompt_ghost::{PendingPromptGhostEscape, PendingReplacedPromptGhostSuffix};
use super::InputRead;

pub(crate) struct RawInputShellRoute {
    pub(crate) main_prompt_gate: MainPromptGate,
    pub(crate) slash_route_enabled: bool,
    pub(crate) zsh_path_prompt_buffering: Option<ZshPathPromptBuffering>,
}

/// Marks Zsh sessions that keep slash candidates Rust-owned until submission.
pub(crate) struct ZshPathPromptBuffering {
    deferred_tab_typeahead: Option<DeferredZshTabTypeahead>,
}

struct DeferredZshTabTypeahead {
    bytes: Vec<u8>,
    deadline: Instant,
}

const ZSH_TAB_TYPEAHEAD_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

impl ZshPathPromptBuffering {
    pub(crate) fn new() -> Self {
        Self {
            deferred_tab_typeahead: None,
        }
    }

    pub(in crate::raw_input) fn defer_tab_typeahead(&mut self, bytes: &[u8]) {
        if let Some(pending) = self.deferred_tab_typeahead.as_mut() {
            pending.bytes.extend_from_slice(bytes);
        } else {
            self.deferred_tab_typeahead = Some(DeferredZshTabTypeahead {
                bytes: bytes.to_vec(),
                deadline: Instant::now() + ZSH_TAB_TYPEAHEAD_DELAY,
            });
        }
    }

    pub(in crate::raw_input) fn has_deferred_tab_typeahead(&self) -> bool {
        self.deferred_tab_typeahead.is_some()
    }

    pub(in crate::raw_input) fn tab_typeahead_deadline(&self) -> Option<Instant> {
        self.deferred_tab_typeahead
            .as_ref()
            .map(|pending| pending.deadline)
    }

    fn take_due_tab_typeahead(&mut self, force: bool, now: Instant) -> Option<Vec<u8>> {
        if !force
            && self
                .deferred_tab_typeahead
                .as_ref()
                .is_none_or(|pending| now < pending.deadline)
        {
            return None;
        }
        self.deferred_tab_typeahead
            .take()
            .map(|pending| pending.bytes)
    }
}

impl RawInputShellRoute {
    pub(crate) fn new(
        main_prompt_gate: MainPromptGate,
        slash_route_enabled: bool,
        zsh_path_prompt_buffering: Option<ZshPathPromptBuffering>,
    ) -> Self {
        Self {
            main_prompt_gate,
            slash_route_enabled,
            zsh_path_prompt_buffering,
        }
    }
}

#[derive(Default)]
pub(in super::super) struct RawInputRelayState {
    pub(super) card_state: CardInputState,
    pub(super) line_buffer: CandidateLineBuffer,
    pub(super) native_line_state: NativeLineState,
    pub(super) exit_tracker: ExplicitExitTracker,
    pub(super) input_generation: UserPtyInputGeneration,
    pub(super) line_submits: LineSubmitCounter,
    pub(super) main_prompt_gate: MainPromptGate,
    /// Routes exact slash submissions through bash for native history
    /// recall (issue #1718); gated further by `main_prompt_gate` at
    /// submission time.
    pub(super) slash_route_enabled: bool,
    pub(super) zsh_path_prompt_buffering: Option<ZshPathPromptBuffering>,
    pub(super) pending_prompt_ghost_escape: Option<PendingPromptGhostEscape>,
    pub(super) pending_delay_escape: Option<PendingDelayEscape>,
    pub(super) pending_assistance_escape: Option<PendingAssistanceEscape>,
    pub(super) pending_replaced_prompt_ghost_suffix: Option<PendingReplacedPromptGhostSuffix>,
    pub(super) capture_owned_input: CaptureOwnedInput,
    pub(super) deferred_input: Option<InputRead>,
    /// Deadline for a bare ESC held inside the draft card (#1721): on
    /// expiry the relay injects a second ESC which resolves to a cancel.
    pub(super) pending_draft_escape_deadline: Option<std::time::Instant>,
}

impl RawInputRelayState {
    pub(in crate::raw_input) fn with_generation_and_gate(
        input_generation: UserPtyInputGeneration,
        main_prompt_gate: MainPromptGate,
        slash_route_enabled: bool,
    ) -> Self {
        Self {
            input_generation,
            main_prompt_gate,
            slash_route_enabled,
            ..Self::default()
        }
    }

    pub(in crate::raw_input) fn with_shell_route(
        input_generation: UserPtyInputGeneration,
        route: RawInputShellRoute,
    ) -> Self {
        Self {
            input_generation,
            main_prompt_gate: route.main_prompt_gate,
            slash_route_enabled: route.slash_route_enabled,
            zsh_path_prompt_buffering: route.zsh_path_prompt_buffering,
            ..Self::default()
        }
    }
}
pub(in crate::raw_input) fn input_relay_context<'a>(
    master: &'a mut File,
    input_classifier: &'a InputClassifier,
    input_events: &'a dyn RawInputEventSink,
    input_mode: &'a Arc<Mutex<RawInputMode>>,
    state: &'a mut RawInputRelayState,
) -> InputRelayContext<'a> {
    InputRelayContext {
        master,
        input_classifier,
        input_events,
        input_mode,
        input_generation: &state.input_generation,
        line_submits: &mut state.line_submits,
        line_buffer: &mut state.line_buffer,
        native_line_state: &mut state.native_line_state,
        exit_tracker: &mut state.exit_tracker,
        main_prompt_gate: &state.main_prompt_gate,
        slash_route_enabled: state.slash_route_enabled,
        zsh_path_prompt_buffering: state.zsh_path_prompt_buffering.as_mut(),
    }
}

pub(in crate::raw_input) fn flush_deferred_zsh_tab_typeahead(
    force: bool,
    now: Instant,
    master: &mut File,
    input_classifier: &InputClassifier,
    input_events: &dyn RawInputEventSink,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> std::io::Result<()> {
    let Some(bytes) = state
        .zsh_path_prompt_buffering
        .as_mut()
        .and_then(|buffering| buffering.take_due_tab_typeahead(force, now))
    else {
        return Ok(());
    };
    let mut relay = input_relay_context(master, input_classifier, input_events, input_mode, state);
    replay_deferred_zsh_tab_typeahead(&bytes, &mut relay)
}

// A bare ESC inside the draft card waits this long for a split CR/LF
// (legacy Alt+Enter) before it resolves to an explicit cancel (#1721).
const DRAFT_ESCAPE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(50);

/// Arms/disarms the draft-card ESC hold deadline from the card state; runs
/// once per relay loop turn before the next blocking receive (#1721).
pub(super) fn sync_pending_draft_escape(state: &mut RawInputRelayState) {
    if state.card_state.draft_escape_pending() {
        if state.pending_draft_escape_deadline.is_none() {
            state.pending_draft_escape_deadline = Some(Instant::now() + DRAFT_ESCAPE_TIMEOUT);
        }
    } else {
        state.pending_draft_escape_deadline = None;
    }
}

/// On expiry, injects a second ESC into the capture: combined with the held
/// first ESC it resolves to the explicit ESC+ESC cancel path, reusing the
/// normal release/mode bookkeeping (#1721). When the injected ESC releases
/// the capture, the Submitted -> Draining chain must drain here as well
/// (#1932): left pending, the quarantine would swallow the next keystroke
/// typed after the cancel (e.g. the first Up arrow).
pub(super) fn flush_pending_draft_escape(
    now: Instant,
    master: &mut File,
    input_classifier: &InputClassifier,
    input_events: &dyn RawInputEventSink,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> std::io::Result<()> {
    let Some(deadline) = state.pending_draft_escape_deadline else {
        return Ok(());
    };
    if now < deadline {
        return Ok(());
    }
    state.pending_draft_escape_deadline = None;
    let mode = current_raw_input_mode(input_mode);
    if let RawInputMode::Capture {
        capture,
        generation,
        ..
    } = mode
    {
        let result = consume_captured_input(
            &mut state.card_state,
            &capture,
            generation,
            b"\x1b",
            input_events,
            input_mode,
        );
        if result.generation.is_some() {
            let RawInputRelayState {
                card_state,
                line_buffer,
                native_line_state,
                exit_tracker,
                capture_owned_input,
                deferred_input,
                input_generation,
                line_submits,
                main_prompt_gate,
                slash_route_enabled,
                zsh_path_prompt_buffering,
                ..
            } = state;
            let mut relay = InputRelayContext {
                master,
                input_classifier,
                input_events,
                input_mode,
                input_generation,
                line_submits,
                line_buffer,
                native_line_state,
                exit_tracker,
                main_prompt_gate,
                slash_route_enabled: *slash_route_enabled,
                zsh_path_prompt_buffering: zsh_path_prompt_buffering.as_mut(),
            };
            relay.line_buffer.clear();
            relay.native_line_state.clear();
            drain_capture_submission(
                result,
                card_state,
                capture_owned_input,
                deferred_input,
                None,
                &mut relay,
            )?;
        }
    }
    Ok(())
}
