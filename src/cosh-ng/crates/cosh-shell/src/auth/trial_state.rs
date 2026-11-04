use crate::types::EcsTrialProgress;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrialPhase {
    Waiting,
    Pausing,
    Settings,
    Cancelling,
    Closed,
    Failed,
}

#[derive(Debug)]
pub(super) struct TrialFlow {
    pub(super) progress: EcsTrialProgress,
    pub(super) phase: TrialPhase,
    pub(super) selected: usize,
}

impl TrialFlow {
    pub(super) fn new(progress: EcsTrialProgress) -> Self {
        Self {
            progress,
            phase: TrialPhase::Waiting,
            selected: 0,
        }
    }
    pub(super) fn settings(&mut self) {
        self.phase = TrialPhase::Pausing;
        self.progress.revision = self.progress.revision.saturating_add(1);
    }
    pub(super) fn observe(&mut self, progress: EcsTrialProgress) -> bool {
        if progress.run_id != self.progress.run_id
            || progress.flow_id != self.progress.flow_id
            || progress.revision < self.progress.revision
        {
            return false;
        }
        if matches!(self.phase, TrialPhase::Closed | TrialPhase::Failed) {
            return false;
        }
        if matches!(
            self.phase,
            TrialPhase::Pausing | TrialPhase::Settings | TrialPhase::Cancelling
        ) {
            if !progress.local_terminated {
                return false;
            }
            if self.phase == TrialPhase::Pausing {
                self.phase = TrialPhase::Settings;
            } else if self.phase == TrialPhase::Cancelling {
                self.phase = TrialPhase::Closed;
            }
        }
        self.progress = progress;
        true
    }
    pub(super) fn can_resume(&self) -> bool {
        self.phase == TrialPhase::Settings
            && self.progress.local_terminated
            && self.progress.resume_allowed
            && self.progress.dispatch_state == "not_dispatched"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn progress(
        revision: u64,
        phase: &str,
        dispatch: &str,
        local_terminated: bool,
    ) -> EcsTrialProgress {
        EcsTrialProgress {
            run_id: "opaque".into(),
            flow_id: "flow".into(),
            revision,
            phase: phase.into(),
            code: None,
            dispatch_state: dispatch.into(),
            local_terminated,
            resume_allowed: dispatch == "not_dispatched" && local_terminated,
        }
    }
    #[test]
    fn settings_rejects_late_ready_until_local_pause_ack() {
        let mut flow = TrialFlow::new(progress(1, "waiting_activation", "not_dispatched", false));
        flow.settings();
        assert_eq!(flow.phase, TrialPhase::Pausing);
        assert!(!flow.observe(progress(1, "ready", "not_dispatched", false)));
        assert!(flow.observe(progress(2, "paused", "not_dispatched", true)));
        assert_eq!(flow.phase, TrialPhase::Settings);
        assert!(flow.can_resume());
    }
    #[test]
    fn accepted_unknown_can_close_future_use_but_cannot_resume() {
        for dispatch in ["accepted", "unknown"] {
            let mut flow = TrialFlow::new(progress(1, "waiting_activation", dispatch, false));
            flow.settings();
            assert!(flow.observe(progress(2, "paused", dispatch, true)));
            assert_eq!(flow.phase, TrialPhase::Settings);
            assert!(!flow.can_resume());
        }
    }
    #[test]
    fn different_flow_or_run_and_old_revision_never_replace_current_state() {
        let mut flow = TrialFlow::new(progress(4, "waiting_activation", "not_dispatched", false));
        assert!(!flow.observe(progress(3, "ready", "not_dispatched", false)));
        let mut wrong = progress(5, "ready", "not_dispatched", false);
        wrong.run_id = "other".into();
        assert!(!flow.observe(wrong));
        let mut wrong = progress(5, "ready", "not_dispatched", false);
        wrong.flow_id = "other".into();
        assert!(!flow.observe(wrong));
    }
}
