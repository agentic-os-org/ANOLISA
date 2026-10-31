use super::{ApprovalResult, CoshCore};

pub(super) struct ApprovalExposure {
    memory_before: bool,
    checkpoint_before: bool,
    pub(super) known_unexecuted: bool,
    unexecuted_before_wait: bool,
}

impl ApprovalExposure {
    pub(super) fn observe(&mut self, result: &ApprovalResult) {
        self.known_unexecuted = self.unexecuted_before_wait
            && matches!(result, ApprovalResult::Allowed | ApprovalResult::Denied(_));
    }

    pub(super) fn executing(&mut self) {
        self.known_unexecuted = false;
    }
}

impl CoshCore {
    pub(super) fn begin_approval_exposure(
        &mut self,
        exposure: &mut Option<ApprovalExposure>,
    ) -> Result<(), String> {
        let memory_before = self.failed_side_effect_pending_approval;
        let checkpoint_before = self.arm_reapproval_checkpoint()?;
        let exposure = exposure.get_or_insert(ApprovalExposure {
            memory_before,
            checkpoint_before,
            known_unexecuted: true,
            unexecuted_before_wait: true,
        });
        exposure.unexecuted_before_wait = exposure.known_unexecuted;
        exposure.known_unexecuted = false;
        self.failed_side_effect_pending_approval = true;
        Ok(())
    }

    pub(super) fn restore_unexecuted_exposure(
        &mut self,
        exposure: &ApprovalExposure,
    ) -> Result<(), String> {
        if exposure.known_unexecuted {
            if !exposure.checkpoint_before {
                self.restore_unexecuted_checkpoint()?;
            }
            self.failed_side_effect_pending_approval = exposure.memory_before;
        }
        Ok(())
    }

    pub(super) fn arm_reapproval_checkpoint(&self) -> Result<bool, String> {
        let Some(checkpoint) = self.reapproval_checkpoint.as_ref() else {
            return Ok(false);
        };
        let pending = checkpoint.pending().map_err(|error| format!("{error:?}"))?;
        checkpoint
            .set_pending(true)
            .map_err(|error| format!("{error:?}"))?;
        Ok(pending)
    }

    pub(super) fn restore_unexecuted_checkpoint(&self) -> Result<(), String> {
        if let Some(checkpoint) = self.reapproval_checkpoint.as_ref() {
            checkpoint
                .set_pending(false)
                .map_err(|error| format!("{error:?}"))?;
        }
        Ok(())
    }
}
