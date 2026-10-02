//! Runtime compaction bookkeeping for a live engine: the active projection,
//! provider-reported token accounting, and effective-context estimation.

use crate::provider::Message;

use super::budget::{estimate_messages_tokens, estimate_text_tokens};
use super::projection::{effective_messages, CompactionState};

/// Tokens reserved on top of the measurable prefix for context that is
/// injected later in a run (hook context, asynchronously loaded skill
/// summaries), so the budget never assumes a smaller prefix than the
/// provider will actually see.
const PREFIX_RESERVE_TOKENS: u64 = 1024;

/// Conservative runtime-prefix (`P`) estimate for budget computations.
///
/// The caller renders the system prompt and serialized tool declarations;
/// this owns the token math and the fixed reserve for late-injected context.
pub(crate) fn estimate_prefix_tokens(system_prompt: &str, tool_declarations_json: &str) -> u64 {
    estimate_text_tokens(system_prompt)
        + estimate_text_tokens(tool_declarations_json)
        + PREFIX_RESERVE_TOKENS
}

/// Compaction runtime state owned by a live engine.
///
/// Pairs the active projection with the provider-reported token accounting
/// that prices it, so the two can never drift apart: committing a new
/// projection atomically invalidates the stale usage measurement.
#[derive(Default)]
pub struct CompactionRuntime {
    /// Active compaction projection over the transcript prefix, if any.
    ///
    /// The engine's transcript always stays complete; the provider only sees
    /// the projected effective context.
    state: Option<CompactionState>,
    /// Highest revision this session has committed, independent of whether
    /// the projection that carried it is still active.
    ///
    /// Kept separate from `state` so a projection rejected on load cannot let
    /// the next emergency compaction reuse a published revision.
    revision: u64,
    /// Provider-reported prompt tokens from the most recent request.
    last_prompt_tokens: Option<u64>,
}

impl CompactionRuntime {
    /// The active projection, if any.
    pub fn state(&self) -> Option<&CompactionState> {
        self.state.as_ref()
    }

    /// The monotonic compaction revision clock; `0` before the first commit.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Replaces the projection and revision clock with values loaded from a
    /// persisted session.
    ///
    /// The clock is passed separately because `state` may have been dropped by
    /// sanitization while its revision remains published.
    ///
    /// Any provider-reported usage measured the previous projection and is
    /// discarded with it.
    pub fn load_state(&mut self, state: Option<CompactionState>, revision: u64) {
        self.state = state;
        self.revision = revision;
        self.last_prompt_tokens = None;
    }

    /// Commits a freshly produced projection and advances the revision clock.
    ///
    /// The last provider-reported usage measured the pre-compaction context
    /// and must not suppress the shrunken estimate, so it is cleared here.
    pub(super) fn commit_state(&mut self, state: CompactionState) {
        self.revision = state.revision;
        self.state = Some(state);
        self.last_prompt_tokens = None;
    }

    /// Records the provider-reported prompt size of the latest request.
    ///
    /// This is the only hand-off point for provider usage: keeping it behind
    /// an explicit API ties the measurement to the projection it priced.
    pub fn note_provider_usage(&mut self, prompt_tokens: u64) {
        self.last_prompt_tokens = Some(prompt_tokens);
    }

    /// Provider-visible view of the transcript under the active projection.
    pub(crate) fn effective_messages(&self, messages: &[Message]) -> Vec<Message> {
        effective_messages(messages, self.state.as_ref())
    }

    /// Current effective-context size in tokens.
    ///
    /// Keeps the larger of the local estimate and the provider-reported
    /// input size so estimation error can never under-report pressure.
    pub(crate) fn effective_history_tokens(&self, messages: &[Message], prefix_tokens: u64) -> u64 {
        let estimated = estimate_messages_tokens(&self.effective_messages(messages));
        match self.last_prompt_tokens {
            Some(reported) => estimated.max(reported.saturating_sub(prefix_tokens)),
            None => estimated,
        }
    }
}
