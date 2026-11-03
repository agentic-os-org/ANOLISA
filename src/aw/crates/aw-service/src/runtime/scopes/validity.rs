//! Bridge failure fences execution while the native owner retains cleanup responsibility.

use super::*;

const WINDOW: Duration = Duration::from_secs(15);

pub(super) struct Validity {
    pub(super) deadline: Instant,
    failure: Option<&'static str>,
}

impl Validity {
    pub(super) fn check(&self) -> Result<()> {
        if let Some(code) = self.failure {
            return Err(rejected(code));
        }
        if Instant::now() >= self.deadline {
            return Err(rejected("scope_bridge_expired"));
        }
        Ok(())
    }
}

impl Owner {
    pub(in crate::runtime) fn validity_status(&self) -> Value {
        let alive = self.process.check().is_ok();
        let state = if !alive || self.cancelled.load(Ordering::Acquire) {
            "unavailable"
        } else if self.acknowledge_by.is_some() {
            "pending"
        } else {
            match &self.validity {
                None => "unmonitored",
                Some(validity) if validity.check().is_ok() => "ready",
                Some(_) => "failed",
            }
        };
        json!({"native_alive": alive, "covered_bridge": state})
    }
}

impl Runtime {
    pub(in crate::runtime) fn watch_scope(&self, id: &str, pid: u32) -> Result<Value> {
        self.check_scope_owner(id)?;
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
            .get_mut(id)
            .filter(|owner| owner.parent.is_some())
            .ok_or_else(|| rejected("scope_not_task"))?;
        owner.process.matches(pid)?;
        if owner.cancelled.load(Ordering::Acquire) || owner.acknowledge_by.is_some() {
            return Err(rejected("scope_closing"));
        }
        if owner.validity.is_some() {
            return Err(rejected("scope_already_watched"));
        }
        self.lifecycle_record(
            "scope_watched",
            id,
            json!({"policy_revision": instance.host.revision()}),
        )?;
        owner.validity = Some(Validity {
            deadline: Instant::now() + WINDOW,
            failure: None,
        });
        Ok(json!({"policy_revision": instance.host.revision(), "interval_ms": 2000}))
    }

    pub(in crate::runtime) fn check_scope(
        &self,
        id: &str,
        pid: u32,
        revision: &str,
        valid: bool,
    ) -> Result<Value> {
        self.check_scope_owner(id)?;
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
            .get_mut(id)
            .ok_or_else(|| rejected("scope_unavailable"))?;
        owner.process.matches(pid)?;
        if owner.cancelled.load(Ordering::Acquire) {
            return Err(rejected("scope_closing"));
        }
        let validity = owner
            .validity
            .as_mut()
            .ok_or_else(|| rejected("scope_not_watched"))?;
        validity.check()?;
        if !valid || revision != instance.host.revision() {
            validity.failure = Some("scope_bridge_invalid");
            self.lifecycle_record(
                "scope_bridge_invalid",
                id,
                json!({"policy_revision": instance.host.revision()}),
            )?;
            return Err(rejected("scope_bridge_invalid"));
        }
        validity.deadline = Instant::now() + WINDOW;
        Ok(json!({"valid": true}))
    }

    pub(in crate::runtime) fn maintain_validity(&self) -> Result<()> {
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        let events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
        for event in events.values() {
            if owners
                .get(&event.instance_id)
                .and_then(|owner| owner.validity.as_ref())
                .is_some_and(|validity| validity.check().is_err())
            {
                event.cancelled.store(true, Ordering::Release);
            }
        }
        Ok(())
    }
}
