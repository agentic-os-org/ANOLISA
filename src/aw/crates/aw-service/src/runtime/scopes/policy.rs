//! Task snapshots and reload admission share the instance gate with scope reservation.

use super::*;

impl Runtime {
    pub(in crate::runtime) fn resident_root(&self, id: &str) -> Result<bool> {
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        match owners.get(id) {
            Some(owner) if owner.parent.is_some() => Err(rejected("scope_not_root")),
            Some(owner) => {
                if owner.cancelled.load(Ordering::Acquire) {
                    return Err(rejected("scope_closing"));
                }
                owner.process.check()?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(in crate::runtime) fn corroborate_scope_root(&self, id: &str, pid: u32) -> Result<()> {
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        owners
            .get(id)
            .filter(|owner| owner.parent.is_none())
            .ok_or_else(|| rejected("scope_parent_unowned"))?
            .process
            .corroborate(pid)
    }

    pub(in crate::runtime) fn no_scope_children(&self, id: &str) -> Result<()> {
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        if owners
            .values()
            .any(|owner| owner.parent.as_deref() == Some(id))
        {
            return Err(rejected("busy"));
        }
        Ok(())
    }

    pub(in crate::runtime) fn scope_policy(&self, id: &str) -> Result<Value> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instances
            .get(id)
            .and_then(Option::as_ref)
            .filter(|instance| !instance.closing)
            .ok_or_else(|| rejected("scope_unavailable"))?;
        let mut owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        let owner = owners
            .get(id)
            .ok_or_else(|| rejected("scope_unavailable"))?;
        let parent = owner
            .parent
            .as_ref()
            .ok_or_else(|| rejected("scope_not_task"))?;
        let root = owners
            .get(parent)
            .ok_or_else(|| rejected("scope_parent_unavailable"))?;
        if owner.cancelled.load(Ordering::Acquire)
            || root.cancelled.load(Ordering::Acquire)
            || owner
                .acknowledge_by
                .is_some_and(|deadline| Instant::now() >= deadline)
            || (owner.acknowledge_by.is_none() && !owner.policy_confirmed)
        {
            return Err(rejected("scope_acknowledgement_expired"));
        }
        owner.process.check()?;
        root.process.check()?;
        if !instances
            .get(parent)
            .and_then(Option::as_ref)
            .is_some_and(|root| {
                !root.closing && root.reload.as_ref().is_some_and(|reload| reload.resident)
            })
        {
            return Err(rejected("scope_reload_unsupported"));
        }
        let mut snapshot = aw_adapter::reload::scope_policy(
            instance.host.configuration().as_value(),
            instance.host.target(),
            instance.host.steps(),
            instance.host.capabilities(),
        )
        .map_err(|_| rejected("native_policy_unsupported"))?;
        snapshot["policy_revision"] = json!(instance.host.revision());
        owners
            .get_mut(id)
            .ok_or_else(|| rejected("scope_unavailable"))?
            .policy_confirmed = true;
        Ok(snapshot)
    }
}
