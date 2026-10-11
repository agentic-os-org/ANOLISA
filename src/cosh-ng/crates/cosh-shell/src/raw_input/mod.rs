use crate::input::InterceptReason;

mod capture_bridge;
mod card_capture;
mod draft_editor;
mod event_parser;
mod event_sender;

pub(crate) use draft_editor::PromptDraftEditor;
mod generation;
mod mode;
mod path_prompt_candidate;
mod prompt_epoch;
mod pty;
mod relay;
mod relay_action;
mod soft_newline;
mod spawn;

pub(crate) use event_parser::redact_extension_setting_value;
pub(crate) use generation::UserPtyInputGeneration;
pub(crate) use mode::{update_input_mode, update_locked_input_mode, RawInputMode};
pub use mode::{PromptGhostCandidate, PromptGhostRoute, RawInputCapture, RawObserverAction};
pub(crate) use prompt_epoch::PromptEpochExchange;
pub(crate) use pty::{
    foreground_process_group_for_fds, process_group_exists, set_pty_winsize,
    signal_foreground_process_group, signal_process_group, signal_process_group_id, write_all_pty,
};
pub use relay_action::RawRelayAction;
pub(crate) use spawn::{
    spawn_raw_action_relay, spawn_raw_action_relay_with_wake, spawn_raw_input_relay,
    spawn_raw_input_relay_with_wake, RawInputShellRoute, ZshPathPromptBuffering,
};

pub(super) const CTRL_C: u8 = 0x03;
pub(super) const CTRL_U: u8 = 0x15;
pub(super) const ESC: u8 = 0x1b;

mod main_prompt_gate;
pub(crate) use main_prompt_gate::MainPromptGate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawInputEvent {
    ShellInputActivity {
        empty: bool,
    },
    /// User bytes the relay wrote to the PTY: the write generation plus how
    /// many recognized line submissions the write carried. Anchors prompt
    /// replay state so real user input expires stale replays.
    PtyUserWrite {
        generation: u64,
        line_submits: usize,
    },
    CtrlC,
    Esc,
    CandidateRedraw {
        input: Vec<u8>,
        hint: Option<String>,
    },
    CandidateCommit(Vec<u8>),
    PromptGhostClear,
    PromptGhostAccepted {
        suggestion_id: Option<String>,
    },
    PromptGhostCycle {
        text: String,
    },
    PromptGhostDismissed,
    /// Shift+Tab changed whether the Enhanced session may route input to AI.
    AssistanceToggled,
    PromptGhostIntercept {
        input: String,
        suggestion_id: Option<String>,
    },
    CandidateClearLine,
    UserIntercept(String, InterceptReason),
    UserInterceptWithRouting {
        input: String,
        reason: InterceptReason,
        cwd: String,
        sensitive: bool,
    },
    /// A missing-path prompt withheld from shell execution, so the runtime
    /// must acknowledge it on a stable UI surface.
    NativePathPromptIntercept {
        input: String,
        cwd: String,
        sensitive: bool,
    },
    /// Delivers a queued control only after all preceding shell submissions
    /// in the same input batch reach their primary prompts.
    UserInterceptAtPrompt {
        input: String,
        reason: InterceptReason,
        pending_submits: usize,
    },
    /// A whitelisted soft-newline shortcut was observed on a passthrough
    /// path (candidate buffer inactive). Observe-only: the bytes were still
    /// relayed to the shell unchanged; downstream may surface a one-time
    /// discoverability tip at the next prompt-ready (#1721).
    SoftNewlineShortcutObserved,
    /// A multi-line bracketed paste was relayed straight to bash (#1932):
    /// feeds the failure-insight multi-line entry hint, observe-only.
    MultilinePasteObserved,
    /// Input ended while the Shell-owned line could not be proven empty.
    /// The host must terminate the PTY session out-of-band; writing `exit`
    /// here could append to and execute the user's partial line.
    EofShutdownRequested,
    /// #1721 D13: the first soft newline in a candidate upgrades the draft
    /// into the multi-line prompt card; carries the buffered text.
    PromptDraftOpen {
        text: String,
    },
    /// Draft card state snapshot after an editing keystroke (D14).
    PromptDraftChanged {
        id: String,
        text: String,
        viewport: Box<draft_editor::DraftViewport>,
        line_count: usize,
        selected_completion: usize,
    },
    /// Enter inside the draft card: submit the multi-line prompt.
    PromptDraftSubmit {
        id: String,
        text: String,
        slash: bool,
        workspace_cwd: Option<String>,
    },
    /// Esc/Ctrl+C inside the draft card: cancel composition (D15).
    PromptDraftCancel {
        id: String,
    },
    /// The soft-newline upgrade submitted a synthetic empty line so bash
    /// repaints PS1 (#1932); its visually blank accept echo is dropped
    /// at the next prompt boundary instead of surfacing as a blank line.
    SyntheticPromptRepaint,
    CaptureSubmitted {
        kind: &'static str,
        target_id: String,
        generation: u64,
    },
    CaptureDrained {
        generation: u64,
    },
    CaptureExpired {
        generation: u64,
    },
    CaptureOverflow {
        generation: u64,
    },
    /// Quarantined submit-window bytes were discarded on an unsafe chain
    /// terminal state (follow-up card, invalidated chain, late arrival);
    /// the runtime renders a visible rejection notice (#1913).
    CaptureInputRejected {
        generation: u64,
        byte_len: usize,
    },
    CardFocus(String, usize),
    CardToggle(String, usize),
    CardInput(String, String),
    CardSecretInput(String, String),
    CardApprove(String),
    CardApproveTurn(String),
    CardAlwaysTrust(String),
    CardDeny(String),
    CardDetails(String),
    CardCancel(String),
    CardAnswer(String),
    QuestionSubmitAttempt(String),
    CardSecretAnswer(String),
    QuestionCancel(String),
    /// Ctrl+C on a question capture: abandon the whole prompt.
    ///
    /// Distinct from [`RawInputEvent::QuestionCancel`] (ESC) only so a multi-step prompt can let
    /// ESC step back while Ctrl+C keeps its usual meaning. Panels with a single step treat both
    /// identically.
    QuestionAbort(String),
    EvidenceSend(String),
    EvidenceIgnore(String),
    EvidenceCancel(String),
    ModeFocus(String, usize),
    ModeSet(String, usize),
    ModeCancel(String),
    ConfigFocus(String, usize),
    ConfigSave(String),
    ConfigCancel(String),
    ConfigLanguageFocus(String, usize),
    ConfigLanguageSet(String, usize),
    ConfigLanguageCancel(String),
    SessionFocus(String, usize),
    SessionToggle(String, usize),
    SessionResume(String, usize),
    SessionDelete(String),
    SessionClearConfirm(String),
    SessionCancel(String),
}

#[cfg(test)]
mod tests;
