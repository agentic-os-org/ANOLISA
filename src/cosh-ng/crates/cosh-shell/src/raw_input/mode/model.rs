//! Raw-input mode state and observer action types.

use crate::types::ShellHandoffRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawObserverAction {
    Continue,
    RawPassthrough,
    HoldShellOutput,
    DelayShellOutput,
    CaptureInput(RawInputCapture),
    EmitToPty(ShellHandoffRequest),
    EmitToPtyWithPromptRestore(ShellHandoffRequest),
    InterruptForeground,
    RestorePrompt {
        ghost_text: Option<String>,
        ghost_route: PromptGhostRoute,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptGhostRoute {
    NativeShell,
    AgentIntercept {
        suggestion_id: Option<String>,
    },
    AgentSelection {
        candidates: Vec<PromptGhostCandidate>,
        active: usize,
    },
}

/// Semantic destination of a [`PromptGhostRoute`], stripped of display-only
/// payload (ghost text, candidate list, active index). Routes with the same
/// kind interpret Tab/Enter identically, so kind changes mark an input
/// ownership cutover while same-kind refreshes do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptGhostRouteKind {
    NativeShell,
    AgentIntercept,
    AgentSelection,
}

impl PromptGhostRoute {
    /// Exhaustive on purpose (no wildcard arm): adding a `PromptGhostRoute`
    /// variant fails to compile here until the new variant receives its own
    /// ownership classification, so a future route can never be silently
    /// folded into an existing kind.
    pub(crate) fn kind(&self) -> PromptGhostRouteKind {
        match self {
            Self::NativeShell => PromptGhostRouteKind::NativeShell,
            Self::AgentIntercept { .. } => PromptGhostRouteKind::AgentIntercept,
            Self::AgentSelection { .. } => PromptGhostRouteKind::AgentSelection,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptGhostCandidate {
    pub text: String,
    pub suggestion_id: String,
}

impl Default for PromptGhostRoute {
    fn default() -> Self {
        Self::AgentIntercept {
            suggestion_id: None,
        }
    }
}

impl RawObserverAction {
    pub(crate) fn hold_shell_output(self) -> bool {
        matches!(
            self,
            Self::HoldShellOutput | Self::DelayShellOutput | Self::CaptureInput(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawInputMode {
    Passthrough,
    RawPassthrough,
    Hold,
    Delay {
        generation: u64,
    },
    PromptGhost {
        text: String,
        route: PromptGhostRoute,
    },
    Capture {
        capture: RawInputCapture,
        generation: u64,
        installed_at: std::time::Instant,
    },
    Submitted {
        capture: RawInputCapture,
        generation: u64,
    },
    Draining {
        previous_capture: RawInputCapture,
        generation: u64,
        next_capture: Option<RawInputCapture>,
        invalidated: bool,
        post_owner: PostCaptureOwner,
    },
    Terminal {
        previous_capture: RawInputCapture,
        generation: u64,
    },
}

/// Input owner acknowledged by the observer for after the capture chain
/// drains. The drain terminal installs this owner (instead of assuming
/// the main prompt) so quarantined submit-window bytes replay with the
/// same routing a live keystroke would get under that owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostCaptureOwner {
    MainPrompt,
    Delay,
    RawPassthrough,
    Hold,
    /// A prompt ghost cannot be reconstructed from the drain terminal;
    /// replaying into its interactive Tab/Enter interpreter could submit
    /// text the user never confirmed, so this owner rejects the replay.
    PromptGhost,
}

impl PostCaptureOwner {
    /// Exhaustive on purpose: a new observer action must pick an owner
    /// classification before it can follow a submitted capture.
    pub(crate) fn from_action(action: &RawObserverAction) -> Self {
        match action {
            RawObserverAction::DelayShellOutput => Self::Delay,
            RawObserverAction::RawPassthrough => Self::RawPassthrough,
            RawObserverAction::HoldShellOutput => Self::Hold,
            RawObserverAction::RestorePrompt {
                ghost_text: Some(_),
                ..
            } => Self::PromptGhost,
            RawObserverAction::CaptureInput(_)
            | RawObserverAction::Continue
            | RawObserverAction::EmitToPty(_)
            | RawObserverAction::EmitToPtyWithPromptRestore(_)
            | RawObserverAction::InterruptForeground
            | RawObserverAction::RestorePrompt {
                ghost_text: None, ..
            } => Self::MainPrompt,
        }
    }
}

/// Input ownership boundary for a [`RawInputMode`]. Display-only updates
/// inside the same owner (prompt ghost candidate cycling, card selection
/// redraws) keep the owner stable, so bytes obtained across such updates are
/// not treated as an ownership cutover and never get silently dropped.
/// A prompt ghost route change (e.g. `AgentSelection` -> `NativeShell`) is
/// not display-only: Tab/Enter have different destinations per route kind,
/// so crossing kinds is an ownership cutover and in-flight bytes are
/// discarded instead of being reinterpreted under the new route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputOwnership {
    Passthrough,
    RawPassthrough,
    Hold,
    Delay(u64),
    PromptGhost(PromptGhostRouteKind),
    Capture(u64),
    Submitted(u64),
    Draining(u64),
    Terminal(u64),
}

impl RawInputMode {
    pub(crate) fn input_ownership(&self) -> InputOwnership {
        match self {
            Self::Passthrough => InputOwnership::Passthrough,
            Self::RawPassthrough => InputOwnership::RawPassthrough,
            Self::Hold => InputOwnership::Hold,
            Self::Delay { generation } => InputOwnership::Delay(*generation),
            Self::PromptGhost { route, .. } => InputOwnership::PromptGhost(route.kind()),
            Self::Capture { generation, .. } => InputOwnership::Capture(*generation),
            Self::Submitted { generation, .. } => InputOwnership::Submitted(*generation),
            Self::Draining { generation, .. } => InputOwnership::Draining(*generation),
            Self::Terminal { generation, .. } => InputOwnership::Terminal(*generation),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawInputCapture {
    Question {
        id: String,
        option_count: usize,
        selected: usize,
        allow_free_text: bool,
        multiple: bool,
        secret: bool,
    },
    /// Editable text-only question whose input buffer starts with `initial_text`.
    TextQuestion {
        id: String,
        initial_text: String,
        secret: bool,
    },
    Approval {
        id: String,
        action_set: crate::ui::ApprovalActionSet,
    },
    Mode {
        id: String,
        option_count: usize,
        selected: usize,
    },
    Config {
        id: String,
        option_count: usize,
        selected: usize,
    },
    ConfigLanguage {
        id: String,
        option_count: usize,
        selected: usize,
    },
    Session {
        id: String,
        option_count: usize,
        selected: usize,
        marked_for_clear: Vec<bool>,
        confirming_clear: bool,
    },
    Consultation {
        id: String,
    },
    Evidence {
        id: String,
    },
    /// Multi-line prompt draft card (#1721 D13): opened by the first soft
    /// newline in a native/escape candidate; the capture owns every key
    /// until submit (Enter) or cancel (Esc/Ctrl+C).
    PromptDraft {
        id: String,
        initial_text: Box<str>,
        /// Replacement and its absolute editor cursor, boxed to keep mode snapshots small.
        completion: Option<Box<(String, (usize, usize))>>,
        /// Enables local slash completion and control-command submission.
        agent_composer: bool,
        /// Composer-owned directory, independent of later shell prompt events.
        workspace_cwd: Option<Box<str>>,
    },
}

impl RawInputCapture {
    pub(super) fn is_session_mark_refresh(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Session {
                    id: current_id,
                    option_count: current_option_count,
                    selected: current_selected,
                    confirming_clear: current_confirming_clear,
                    ..
                },
                Self::Session {
                    id: next_id,
                    option_count: next_option_count,
                    selected: next_selected,
                    confirming_clear: next_confirming_clear,
                    ..
                }
            ) if current_id == next_id
                && current_option_count == next_option_count
                && current_selected == next_selected
                && current_confirming_clear == next_confirming_clear
        )
    }
}
