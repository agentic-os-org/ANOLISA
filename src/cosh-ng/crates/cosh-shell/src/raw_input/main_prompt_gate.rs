//! Shared primary-prompt readiness used by raw input routing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared "shell is sitting at its primary prompt" gate (#1721 D16).
///
/// Prompt markers raise the general gate; submissions and command starts
/// clear it. The startup-only latch lets missing-path routing consume the
/// marker read before relay wiring without changing other shortcut behavior.
#[derive(Clone, Debug, Default)]
pub(crate) struct MainPromptGate {
    at_prompt: Arc<AtomicBool>,
    initial_prompt: Arc<AtomicBool>,
    /// Whether this session has ever shown a prompt (#7897): sessions whose
    /// shell never emits prompt markers can never re-raise the gate, so
    /// gate-held input would only burn its wait cap there.
    ever_ready: Arc<AtomicBool>,
}

impl MainPromptGate {
    pub(crate) fn set_at_prompt(&self, at_prompt: bool) {
        self.at_prompt.store(at_prompt, Ordering::Relaxed);
        if at_prompt {
            self.ever_ready.store(true, Ordering::Relaxed);
        } else {
            self.initial_prompt.store(false, Ordering::Relaxed);
        }
    }

    pub(crate) fn is_at_prompt(&self) -> bool {
        self.at_prompt.load(Ordering::Relaxed)
    }

    pub(crate) fn seed_initial_prompt(&self) {
        self.initial_prompt.store(true, Ordering::Relaxed);
        self.ever_ready.store(true, Ordering::Relaxed);
    }

    /// Whether the gate has risen at least once in this session. Only then
    /// can a deferred remainder held for the gate (#7897) actually be
    /// released by a gate rise; elsewhere holding just delays delivery by
    /// the wait cap for no routing benefit.
    pub(crate) fn has_ever_been_ready(&self) -> bool {
        self.ever_ready.load(Ordering::Relaxed)
    }

    pub(crate) fn is_path_prompt_ready(&self) -> bool {
        self.is_at_prompt() || self.initial_prompt.load(Ordering::Relaxed)
    }
}
