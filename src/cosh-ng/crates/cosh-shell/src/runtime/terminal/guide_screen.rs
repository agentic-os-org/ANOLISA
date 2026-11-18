use std::sync::atomic::{AtomicU64, Ordering};

use crate::ui::guide::ScreenEvent;

pub(super) struct GuideScreenRecovery {
    state: AtomicU64,
}

const ACTIVE: u64 = 1;
const TRANSITIONING: u64 = 2;
const EXITING: u64 = 4;
const SIGNAL_SHIFT: u32 = 3;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ScreenUpdate {
    Continue,
    Stopped,
    WaitForExit,
    Exit(i32),
}

impl GuideScreenRecovery {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    pub(super) fn update(&self, event: ScreenEvent) -> ScreenUpdate {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & EXITING != 0 {
                return ScreenUpdate::WaitForExit;
            }
            let phase = state & (ACTIVE | TRANSITIONING);
            let (next, result) = match event {
                ScreenEvent::Entering if phase == 0 => (TRANSITIONING, ScreenUpdate::Continue),
                ScreenEvent::Drawing | ScreenEvent::Leaving if phase == ACTIVE => {
                    (ACTIVE | TRANSITIONING, ScreenUpdate::Continue)
                }
                ScreenEvent::Active | ScreenEvent::Inactive if phase & TRANSITIONING != 0 => {
                    let active = u64::from(matches!(event, ScreenEvent::Active));
                    let signal = (state >> SIGNAL_SHIFT) as i32;
                    if signal == 0 {
                        (active, ScreenUpdate::Continue)
                    } else {
                        (
                            active | EXITING | ((signal as u64) << SIGNAL_SHIFT),
                            ScreenUpdate::Exit(signal),
                        )
                    }
                }
                _ => return ScreenUpdate::Stopped,
            };
            match self
                .state
                .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return result,
                Err(current) => state = current,
            }
        }
    }

    pub(super) fn defer_signal(&self, signal: i32) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & EXITING != 0 {
                return true;
            }
            let mut next = state;
            if state >> SIGNAL_SHIFT == 0 {
                next |= (signal as u64) << SIGNAL_SHIFT;
            }
            let deferred = state & TRANSITIONING != 0;
            if !deferred {
                next |= EXITING;
            }
            // A signal and the next terminal write compete for the same state.
            match self
                .state
                .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return deferred,
                Err(current) => state = current,
            }
        }
    }

    pub(super) fn take_active(&self) -> bool {
        self.state.fetch_and(!ACTIVE, Ordering::AcqRel) & ACTIVE != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_background_exit_claim_prevents_later_terminal_entry() {
        let state = GuideScreenRecovery::new();
        std::thread::scope(|scope| {
            assert!(!scope
                .spawn(|| state.defer_signal(nix::libc::SIGTERM))
                .join()
                .unwrap());
        });
        assert!(!state.take_active());
        state.update(ScreenEvent::Entering);
        state.update(ScreenEvent::Active);
        assert!(
            !state.take_active(),
            "terminal entry must not overtake signal recovery"
        );
    }

    #[test]
    fn signal_during_entry_waits_until_the_guide_owns_the_screen() {
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        assert!(state.defer_signal(nix::libc::SIGTERM));
        assert!(!state.take_active());
        assert_eq!(
            state.update(ScreenEvent::Active),
            ScreenUpdate::Exit(nix::libc::SIGTERM)
        );
        assert!(state.take_active());
        assert!(!state.take_active());
    }

    #[test]
    fn failed_entry_never_leaves_a_parent_alternate_screen() {
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        assert!(state.defer_signal(nix::libc::SIGTERM));
        assert_eq!(
            state.update(ScreenEvent::Inactive),
            ScreenUpdate::Exit(nix::libc::SIGTERM)
        );
        assert!(!state.take_active());
    }

    #[test]
    fn signal_during_leave_waits_for_cleanup_without_a_second_leave() {
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        state.update(ScreenEvent::Active);
        state.update(ScreenEvent::Leaving);
        assert!(state.defer_signal(nix::libc::SIGTERM));
        assert_eq!(
            state.update(ScreenEvent::Inactive),
            ScreenUpdate::Exit(nix::libc::SIGTERM)
        );
        assert!(!state.take_active());
    }

    #[test]
    fn inline_and_stable_guide_signals_are_not_deferred() {
        let state = GuideScreenRecovery::new();
        assert!(!state.defer_signal(nix::libc::SIGTERM));
        assert!(!state.take_active());
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        state.update(ScreenEvent::Active);
        assert!(!state.defer_signal(nix::libc::SIGTERM));
        assert!(state.take_active());
    }

    #[test]
    fn a_transition_preserves_the_first_pending_signal() {
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        assert!(state.defer_signal(nix::libc::SIGTERM));
        assert!(state.defer_signal(nix::libc::SIGHUP));
        assert_eq!(
            state.update(ScreenEvent::Active),
            ScreenUpdate::Exit(nix::libc::SIGTERM)
        );
        assert_eq!(
            state.update(ScreenEvent::Inactive),
            ScreenUpdate::WaitForExit
        );
    }

    #[test]
    fn finishing_a_transition_and_publishing_a_signal_dispatch_exactly_once() {
        for _ in 0..100 {
            let state = GuideScreenRecovery::new();
            state.update(ScreenEvent::Entering);
            std::thread::scope(|scope| {
                let signal = scope.spawn(|| state.defer_signal(nix::libc::SIGTERM));
                let pending = state.update(ScreenEvent::Active);
                let deferred = signal.join().unwrap();
                assert_eq!(
                    usize::from(!deferred) + usize::from(matches!(pending, ScreenUpdate::Exit(_))),
                    1
                );
            });
        }
    }

    #[test]
    fn claimed_exit_blocks_terminal_draw_and_leave() {
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        state.update(ScreenEvent::Active);
        assert!(!state.defer_signal(nix::libc::SIGTERM));
        for event in [
            ScreenEvent::Drawing,
            ScreenEvent::Leaving,
            ScreenEvent::Entering,
            ScreenEvent::Inactive,
        ] {
            assert_eq!(state.update(event), ScreenUpdate::WaitForExit);
        }
        assert!(state.take_active());
        assert!(!state.take_active());
    }

    #[test]
    fn another_exit_owner_requires_waiting_before_the_ui_can_return() {
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        state.update(ScreenEvent::Active);
        assert!(!state.defer_signal(nix::libc::SIGTERM));
        assert_eq!(
            state.update(ScreenEvent::Drawing),
            ScreenUpdate::WaitForExit
        );
    }

    #[test]
    fn drawing_defers_recovery_until_cursor_writes_are_complete() {
        let state = GuideScreenRecovery::new();
        state.update(ScreenEvent::Entering);
        state.update(ScreenEvent::Active);
        assert_eq!(state.update(ScreenEvent::Drawing), ScreenUpdate::Continue);
        assert!(state.defer_signal(nix::libc::SIGTERM));
        assert_eq!(
            state.update(ScreenEvent::Active),
            ScreenUpdate::Exit(nix::libc::SIGTERM)
        );
        assert!(state.take_active());
    }

    #[test]
    fn beginning_entry_and_claiming_exit_have_one_atomic_winner() {
        for _ in 0..100 {
            let state = GuideScreenRecovery::new();
            std::thread::scope(|scope| {
                let signal = scope.spawn(|| state.defer_signal(nix::libc::SIGTERM));
                let started = state.update(ScreenEvent::Entering);
                let deferred = signal.join().unwrap();
                assert_eq!(matches!(started, ScreenUpdate::Continue), deferred);
                if deferred {
                    assert_eq!(
                        state.update(ScreenEvent::Active),
                        ScreenUpdate::Exit(nix::libc::SIGTERM)
                    );
                } else {
                    assert_eq!(started, ScreenUpdate::WaitForExit);
                }
            });
        }
    }
}
