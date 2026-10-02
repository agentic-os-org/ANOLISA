use std::collections::{HashMap, HashSet, VecDeque};

use super::prelude::{FindingSeverity, HookEngine, HookFinding};

const MAX_HOOK_DISPLAY_EVENTS: usize = 128;

/// Option indices for the hook-action disambiguation panel shown when a hook
/// id collides between the shell layer and the agent layer.
pub(crate) const HOOK_ACTION_SHELL: usize = 0;
pub(crate) const HOOK_ACTION_AGENT: usize = 1;
pub(crate) const HOOK_ACTION_BOTH: usize = 2;
pub(crate) const HOOK_ACTION_OPTION_COUNT: usize = 3;

/// Whether the pending disambiguation is an enable or disable action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookActionKind {
    Enable,
    Disable,
}

/// Pending interactive disambiguation when a hook id collides between the
/// shell layer (`HookEngine`) and the agent layer (`cosh-core registry`).
/// The user picks which layer(s) to enable/disable via a question panel
/// before the action is applied.
#[derive(Debug, Clone)]
pub(crate) struct PendingHookAction {
    pub(crate) panel_id: String,
    pub(crate) hook_id: String,
    pub(crate) action: HookActionKind,
    pub(crate) selected_option: usize,
}

#[derive(Default)]
pub(crate) struct HookRuntimeState {
    pub(crate) handled_command_hooks: HashSet<String>,
    pub(crate) rendered_findings: HashSet<String>,
    pub(crate) findings: Vec<RuntimeHookFinding>,
    pub(crate) engine: HookEngine,
    pub(crate) disabled: HashSet<String>,
    pub(crate) pending_consultation: Option<PendingConsultation>,
    pub(crate) pending_consultation_queue: VecDeque<PendingConsultation>,
    pub(crate) rendered_cards: HashMap<String, HookSuppressionRecord>,
    pub(crate) ignored_cards: HashSet<String>,
    pub(crate) blocks_followed_by_user_input: HashSet<String>,
    pub(crate) muted_targets: HashSet<String>,
    pub(crate) feedback: HashMap<String, HookFeedback>,
    pub(crate) noisy_groups: HashSet<String>,
    pub(crate) display_events: Vec<RuntimeHookDisplayEvent>,
    pub(crate) interruption_budget: HashMap<String, InterruptionBudgetRecord>,
    pub(crate) pending_action: Option<PendingHookAction>,
}

impl HookRuntimeState {
    pub(crate) fn mark_block_followed_by_user_input(&mut self, block_id: impl Into<String>) {
        self.blocks_followed_by_user_input.insert(block_id.into());
    }

    pub(crate) fn block_followed_by_user_input(&self, block_id: &str) -> bool {
        self.blocks_followed_by_user_input.contains(block_id)
    }

    pub(crate) fn record_display_event(&mut self, event: RuntimeHookDisplayEvent) {
        self.display_events.push(event);
        if self.display_events.len() > MAX_HOOK_DISPLAY_EVENTS {
            let drop_count = self.display_events.len() - MAX_HOOK_DISPLAY_EVENTS;
            self.display_events.drain(0..drop_count);
        }
    }
}

pub(crate) fn hook_feedback_group_key(
    topic: &str,
    entity_key: &str,
    command_intent: &str,
) -> String {
    format!("{topic}:{entity_key}:{command_intent}")
}

#[derive(Debug, Clone)]
pub(crate) struct PendingConsultation {
    pub(crate) finding_id: String,
    pub(crate) card_id: String,
    pub(crate) block_id: String,
    pub(crate) command: String,
    pub(crate) output_ref: Option<String>,
    pub(crate) state: PendingConsultationState,
    pub(crate) created_at_ms: u64,
    pub(crate) expires_at_ms: u64,
    pub(crate) ended_at_ms: u64,
    pub(crate) queued_at: std::time::Instant,
    #[allow(dead_code)]
    pub(crate) prompt_hint: String,
    pub(crate) hook_finding: Option<HookFinding>,
    pub(crate) recommended_skill: Option<String>,
    pub(crate) context_hints: Vec<String>,
    pub(crate) suppression_key: String,
    pub(crate) topic: String,
    pub(crate) entity_key: String,
    pub(crate) confidence: String,
    pub(crate) display_reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeHookDisplayEvent {
    pub(crate) action: RuntimeHookDisplayAction,
    pub(crate) finding_id: String,
    pub(crate) command_block_id: String,
    pub(crate) hook_id: String,
    pub(crate) topic: String,
    pub(crate) entity_key: String,
    pub(crate) suppression_key: String,
    pub(crate) display: RuntimeHookDisplay,
    pub(crate) display_reason: String,
    pub(crate) confidence: String,
    pub(crate) ended_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeHookDisplayAction {
    Shown,
    Ignored,
    Analyzed,
    Muted,
    Expired,
    Deferred,
}

impl RuntimeHookDisplayAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Shown => "shown",
            Self::Ignored => "ignored",
            Self::Analyzed => "analyzed",
            Self::Muted => "muted",
            Self::Expired => "expired",
            Self::Deferred => "deferred",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingConsultationState {
    Queued,
    Deferred,
    Displayed,
    Ignored,
    Analyzed,
    Expired,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HookSuppressionRecord {
    pub(crate) severity: FindingSeverity,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InterruptionBudgetRecord {
    pub(crate) last_rendered_at_ms: u64,
    pub(crate) severity: FindingSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookFeedback {
    Noisy,
    Useful,
}

impl HookFeedback {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Noisy => "noisy",
            Self::Useful => "useful",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeHookFinding {
    pub(crate) id: String,
    pub(crate) command_block_id: String,
    pub(crate) command: String,
    pub(crate) output_ref: Option<String>,
    pub(crate) ended_at_ms: u64,
    pub(crate) prompt_hint: String,
    pub(crate) finding_markdown: Option<String>,
    pub(crate) hook_finding: Option<HookFinding>,
    pub(crate) recommended_skill: Option<String>,
    pub(crate) display: RuntimeHookDisplay,
    pub(crate) display_reason: String,
    pub(crate) related_hook_ids: Vec<String>,
    pub(crate) topic: String,
    pub(crate) entity_key: String,
    pub(crate) effective_severity: FindingSeverity,
    pub(crate) confidence: String,
    pub(crate) suppression_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeHookDisplay {
    Silent,
    Hint,
    Consultation,
}

impl RuntimeHookDisplay {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Hint => "hint",
            Self::Consultation => "consultation",
        }
    }
}
