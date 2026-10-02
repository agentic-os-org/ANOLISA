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
}

impl MainPromptGate {
    pub(crate) fn set_at_prompt(&self, at_prompt: bool) {
        self.at_prompt.store(at_prompt, Ordering::Relaxed);
        if !at_prompt {
            self.initial_prompt.store(false, Ordering::Relaxed);
        }
    }

    pub(crate) fn is_at_prompt(&self) -> bool {
        self.at_prompt.load(Ordering::Relaxed)
    }

    pub(crate) fn seed_initial_prompt(&self) {
        self.initial_prompt.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_path_prompt_ready(&self) -> bool {
        self.is_at_prompt() || self.initial_prompt.load(Ordering::Relaxed)
    }
}
