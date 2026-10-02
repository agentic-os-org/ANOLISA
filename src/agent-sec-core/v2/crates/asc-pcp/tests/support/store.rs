use crate::*;
use asc_foundation_types::ResourceId;
use asc_policy_types::binding::BindingView;

pub trait TestStore: BindingStateRepository {
    fn read(&self, id: &ResourceId) -> Result<Option<ReconcileRecord>, StoreError> {
        self.get_binding_state(id)
    }
}
impl<T: BindingStateRepository + ?Sized> TestStore for T {}

pub trait TestAdmission {
    fn compare_exchange_reconcile_intent(
        &self,
        expected: &ExpectedBinding,
        desired: &BindingView,
    ) -> Result<bool, StoreError>;
}
impl TestAdmission for asc_pap_repository_memory::ProcessLocalPapRepository {
    fn compare_exchange_reconcile_intent(
        &self,
        expected: &ExpectedBinding,
        desired: &BindingView,
    ) -> Result<bool, StoreError> {
        use asc_pap::PapRepository;
        let Some(mut before) = self.get_binding_state(&expected.id)? else {
            return Ok(false);
        };
        if !expected.matches(&before.binding) {
            return Ok(false);
        }
        if before.binding == *desired {
            return Ok(true);
        }
        // Simulate a newer independently admitted Apply for stale-result tests.
        // End the old lifecycle first; the product PAP never replaces Applying.
        if before.binding.status.phase == asc_policy_types::binding::BindingStatus::Applying
            && before.binding.spec != desired.spec
        {
            let mut failed = before.clone();
            failed.binding.status.phase = asc_policy_types::binding::BindingStatus::ApplyFailed;
            self.compare_exchange_binding_state(&before, &BindingStateWrite::new(failed.clone()))?;
            before = failed;
        }
        if !matches!(
            desired.status.phase,
            asc_policy_types::binding::BindingStatus::PendingApply
                | asc_policy_types::binding::BindingStatus::PendingDelete
        ) && desired.spec == before.binding.spec
        {
            let mut next = before.clone();
            next.binding.status = desired.status.clone();

            return Ok(self
                .compare_exchange_binding_state(&before, &BindingStateWrite::new(next))?
                == WriteResult::Applied);
        }
        self.update_binding(Some(&before.binding), desired)
            .map(|_| true)
            .map_err(|_| StoreError::Invalid)
    }
}

pub fn write_phase(expected: &BindingStateSnapshot, write: &BindingStateWrite) -> &'static str {
    let Some(next) = &write.next else {
        return "finish";
    };
    if !expected.binding.status.phase.is_reconciling()
        && next
            .status
            .as_ref()
            .is_some_and(|s| s.phase.is_reconciling())
    {
        "claim"
    } else if expected.binding.status.phase.is_reconciling()
        && next
            .status
            .as_ref()
            .is_none_or(|s| s.phase == expected.binding.status.phase)
    {
        "register"
    } else {
        "finish"
    }
}
