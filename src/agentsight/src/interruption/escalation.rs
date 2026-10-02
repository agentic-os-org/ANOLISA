//! DeadLoop auto-kill escalation decision.
//!
//! Splits the "given a detection count, which signal (if any) to send" decision
//! out of `unified.rs::detect_and_store_interruptions` so the SIGTERM→SIGKILL
//! escalation ladder is unit-testable. The decision is pure; the actual signal
//! is sent by `execute_kill_action` through an injected [`ProcessKiller`].

use crate::utils::process::{ProcessKiller, Signal};

/// What the auto-kill ladder should do for one DeadLoop detection.
///
/// `new_count` is the post-insert unresolved DeadLoop count for the conversation
/// (`existing_count + 1`); `kill_after_count` is the configured threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KillAction {
    /// Auto-kill disabled: do nothing.
    Disabled,
    /// Below threshold: log progress, send nothing.
    Wait,
    /// At threshold: send SIGTERM (graceful).
    Terminate,
    /// Past threshold: escalate to SIGKILL.
    Kill,
}

/// Decide which signal the DeadLoop auto-kill ladder should send.
///
/// Pure: mirrors the original inline logic exactly —
/// `new_count = existing_count + 1`; `new_count > threshold` → SIGKILL;
/// `new_count == threshold` → SIGTERM; otherwise wait. When disabled, never acts.
pub(crate) fn decide_kill(
    enabled: bool,
    existing_count: usize,
    kill_after_count: usize,
) -> KillAction {
    if !enabled {
        return KillAction::Disabled;
    }
    let new_count = existing_count + 1;
    if new_count > kill_after_count {
        KillAction::Kill
    } else if new_count == kill_after_count {
        KillAction::Terminate
    } else {
        KillAction::Wait
    }
}

/// Carry out a [`KillAction`]: send the signal via `killer` (when there is a pid)
/// and emit the same log lines as the original inline implementation.
///
/// `new_count` / `kill_after_count` are only used for the log messages. A `None`
/// pid silently skips the signal (preserving the original behavior, where the
/// kill branches were guarded by `if let Some(pid)` with no else).
pub(crate) fn execute_kill_action(
    killer: &dyn ProcessKiller,
    action: KillAction,
    pid: Option<i32>,
    cid: &str,
    new_count: usize,
    kill_after_count: usize,
) {
    match action {
        KillAction::Disabled => {}
        KillAction::Kill => {
            if let Some(pid) = pid {
                log::error!(
                    "DeadLoop auto-kill: escalating to SIGKILL for pid {pid} (conversation={cid}, detections={new_count})"
                );
                if let Err(err) = killer.kill(pid, Signal::Kill) {
                    log::error!("DeadLoop auto-kill: SIGKILL failed for pid {pid}: {err}");
                }
            }
        }
        KillAction::Terminate => {
            if let Some(pid) = pid {
                log::error!(
                    "DeadLoop auto-kill: sending SIGTERM to pid {pid} (conversation={cid}, detections={new_count})"
                );
                if let Err(err) = killer.kill(pid, Signal::Term) {
                    log::error!("DeadLoop auto-kill: SIGTERM failed for pid {pid}: {err}");
                }
            }
        }
        KillAction::Wait => {
            log::warn!(
                "DeadLoop auto-kill: detection {new_count}/{kill_after_count} for conversation {cid}, waiting..."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::process::tests::RecordingKiller;

    // ── decide_kill truth table (threshold=3) ──────────────────────
    #[test]
    fn decide_disabled_regardless_of_count() {
        assert_eq!(decide_kill(false, 0, 3), KillAction::Disabled);
        assert_eq!(decide_kill(false, 5, 3), KillAction::Disabled);
    }

    #[test]
    fn decide_below_threshold_waits() {
        // existing 0 -> new 1 -> Wait; existing 1 -> new 2 -> Wait
        assert_eq!(decide_kill(true, 0, 3), KillAction::Wait);
        assert_eq!(decide_kill(true, 1, 3), KillAction::Wait);
    }

    #[test]
    fn decide_at_threshold_terminates_not_kills() {
        // existing 2 -> new 3 == threshold -> SIGTERM (discriminating boundary)
        assert_eq!(decide_kill(true, 2, 3), KillAction::Terminate);
        assert_ne!(decide_kill(true, 2, 3), KillAction::Kill);
    }

    #[test]
    fn decide_past_threshold_kills_not_terminates() {
        // existing 3 -> new 4 > threshold -> SIGKILL (discriminating boundary)
        assert_eq!(decide_kill(true, 3, 3), KillAction::Kill);
        assert_ne!(decide_kill(true, 3, 3), KillAction::Terminate);
    }

    #[test]
    fn decide_zero_threshold_kills_immediately() {
        // threshold 0: existing 0 -> new 1 > 0 -> Kill
        assert_eq!(decide_kill(true, 0, 0), KillAction::Kill);
    }

    // ── execute_kill_action via RecordingKiller ────────────────────
    #[test]
    fn execute_terminate_sends_sigterm() {
        let killer = RecordingKiller::new();
        execute_kill_action(&killer, KillAction::Terminate, Some(42), "cid", 3, 3);
        assert_eq!(*killer.calls.lock().unwrap(), vec![(42, Signal::Term)]);
    }

    #[test]
    fn execute_kill_sends_sigkill() {
        let killer = RecordingKiller::new();
        execute_kill_action(&killer, KillAction::Kill, Some(42), "cid", 4, 3);
        assert_eq!(*killer.calls.lock().unwrap(), vec![(42, Signal::Kill)]);
    }

    #[test]
    fn execute_wait_sends_nothing() {
        let killer = RecordingKiller::new();
        execute_kill_action(&killer, KillAction::Wait, Some(42), "cid", 2, 3);
        assert!(killer.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn execute_disabled_sends_nothing() {
        let killer = RecordingKiller::new();
        execute_kill_action(&killer, KillAction::Disabled, Some(42), "cid", 1, 3);
        assert!(killer.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn execute_none_pid_skips_signal() {
        // Terminate/Kill with no pid must not signal (preserves original guard).
        let killer = RecordingKiller::new();
        execute_kill_action(&killer, KillAction::Terminate, None, "cid", 3, 3);
        execute_kill_action(&killer, KillAction::Kill, None, "cid", 4, 3);
        assert!(killer.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn execute_swallows_killer_error() {
        // When the killer returns Err (e.g. process already gone), the error is
        // logged but not propagated/panicked — exercises the SIGKILL/SIGTERM
        // failed arms. A regression that unwrapped instead of `if let Err` would
        // panic here.
        let killer = crate::utils::process::tests::FailingKiller;
        execute_kill_action(&killer, KillAction::Kill, Some(42), "cid", 4, 3);
        execute_kill_action(&killer, KillAction::Terminate, Some(42), "cid", 3, 3);
        // Reaching here without panic is the assertion.
    }
}
