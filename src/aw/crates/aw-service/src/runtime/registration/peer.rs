//! Independent roots and tasks authenticate every operation, including legacy RPC routes.

use super::super::{process::Process, Runtime};
use crate::{rejected, Operation, Result};

impl Runtime {
    pub(crate) fn authorize_registration_peer(
        &self,
        operation: &Operation,
        peer: Option<&Process>,
    ) -> Result<()> {
        let id = match operation {
            Operation::OpenScope { parent_id, .. } => parent_id,
            Operation::OwnInstance { instance_id, .. }
            | Operation::ScopePolicy { instance_id }
            | Operation::AcknowledgeScope { instance_id }
            | Operation::WatchScope { instance_id, .. }
            | Operation::CheckScope { instance_id, .. }
            | Operation::EnableInvocations { instance_id }
            | Operation::RegisterInvocation { instance_id, .. }
            | Operation::FinishInvocation { instance_id, .. }
            | Operation::EnableReload { instance_id }
            | Operation::AssociateProcess { instance_id, .. }
            | Operation::BridgeLifecycle { instance_id, .. }
            | Operation::HookPlan { instance_id, .. }
            | Operation::StartObservation { instance_id }
            | Operation::ReportRun { instance_id, .. }
            | Operation::ObserveHookEvent { instance_id, .. }
            | Operation::Unbind { instance_id }
            | Operation::ReleaseInstance { instance_id }
            | Operation::OpenEvent { instance_id, .. }
            | Operation::OpenHookEvent { instance_id, .. }
            | Operation::CloseHookEvent { instance_id, .. }
            | Operation::CloseInputEvent { instance_id, .. }
            | Operation::InvokeStep { instance_id, .. }
            | Operation::CloseEvent { instance_id, .. }
            | Operation::FreezeContext { instance_id, .. } => instance_id,
            Operation::Registration { .. }
            | Operation::Status
            | Operation::RecentExecutions
            | Operation::Bind { .. }
            | Operation::UpdatePolicy { .. }
            | Operation::Audit { .. }
            | Operation::Stop => return Ok(()),
        };
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        let Some(owner) = owners.get(id).filter(|owner| owner.independent) else {
            return Ok(());
        };
        let peer = peer.ok_or_else(|| rejected("registration_peer_mismatch"))?;
        if let Operation::OpenScope { pid, .. } = operation {
            owner.process.corroborate(*pid)?;
            Process::read(*pid)?.caller(peer)?;
        } else {
            owner.process.caller(peer)?;
            if owner.parent.is_none() {
                return Err(rejected("registration_requires_scope"));
            }
        }
        Ok(())
    }
}
