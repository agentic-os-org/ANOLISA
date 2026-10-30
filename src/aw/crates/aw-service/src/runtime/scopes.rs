//! Task bindings retain replay fences until close; the resident host retains no task history.

use super::{process::Process, Instance, Runtime};
use crate::{rejected, Binding, Error, Result};
use aw_host::ProcessContext;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

mod policy;
#[cfg(test)]
mod tests;

pub(super) struct Owner {
    pub(super) parent: Option<String>,
    process: Process,
    cancelled: Arc<AtomicBool>,
    acknowledge_by: Option<Instant>,
    policy_confirmed: bool,
}

impl Runtime {
    pub(super) fn check_scope_owner(&self, id: &str) -> Result<()> {
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        if let Some(owner) = owners.get(id) {
            if owner.acknowledge_by.is_some() {
                return Err(rejected("scope_unacknowledged"));
            }
            if owner.cancelled.load(Ordering::Acquire) {
                return Err(rejected("scope_closing"));
            }
            owner.process.check()?;
            if let Some(parent) = &owner.parent {
                let root = owners
                    .get(parent)
                    .ok_or_else(|| rejected("scope_parent_unavailable"))?;
                if root.cancelled.load(Ordering::Acquire) {
                    return Err(rejected("scope_closing"));
                }
                root.process.check()?;
            }
        }
        Ok(())
    }

    pub(super) fn acknowledge_scope(&self, id: &str) -> Result<Value> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        if !instances
            .get(id)
            .and_then(Option::as_ref)
            .is_some_and(|instance| !instance.closing)
        {
            return Err(rejected("scope_unavailable"));
        }
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
        root.process.check()?;
        owner.process.check()?;
        if !owner.policy_confirmed {
            return Err(rejected("scope_policy_unconfirmed"));
        }
        if root.cancelled.load(Ordering::Acquire)
            || owner.cancelled.load(Ordering::Acquire)
            || !owner
                .acknowledge_by
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Err(rejected("scope_acknowledgement_expired"));
        }
        let metadata = json!({"parent_instance_id": parent,
            "policy_revision": instances.get(id).and_then(Option::as_ref).map(|instance| instance.host.revision())});
        let owner = owners
            .get_mut(id)
            .ok_or_else(|| rejected("scope_unavailable"))?;
        // Commit within the handoff window. Keep admission locked until the
        // durable record completes; an audit failure cancels the committed task.
        owner.acknowledge_by = None;
        if let Err(error) = self.lifecycle_record("scope_acknowledged", id, metadata) {
            owner.cancelled.store(true, Ordering::Release);
            return Err(error);
        }
        Ok(json!({"acknowledged": true}))
    }

    pub(super) fn scope_allows_observation(&self, id: &str) -> Result<bool> {
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        Ok(!owners.get(id).is_some_and(|owner| owner.parent.is_some())
            && !owners
                .values()
                .any(|owner| owner.parent.as_deref() == Some(id)))
    }

    pub(super) fn own_instance(&self, id: &str, pid: u32) -> Result<Value> {
        let process = Process::read(pid)?;
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let _instance = instances
            .get(id)
            .and_then(Option::as_ref)
            .filter(|instance| !instance.closing && instance.reload.is_none())
            .ok_or_else(|| rejected("scope_parent_unavailable"))?;
        let mut owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        if owners.contains_key(id) {
            return Err(rejected("instance_already_owned"));
        }
        let audit_key = self.lifecycle_record("instance_owner", id, process.metadata())?;
        owners.insert(
            id.into(),
            Owner {
                parent: None,
                acknowledge_by: None,
                policy_confirmed: true,
                process,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
        );
        Ok(json!({"owned": true, "audit_key": audit_key}))
    }

    pub(super) fn open_scope(
        &self,
        parent: &str,
        pid: u32,
        context: ProcessContext,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let process = Process::read(pid)?;
        let id = self.identifier()?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let (bytes, target, capabilities) = {
            // Match close's lock order. The reservation is visible before preparation starts.
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let mut owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get(parent)
                .and_then(Option::as_ref)
                .filter(|instance| {
                    !instance.closing
                        && instance
                            .reload
                            .as_ref()
                            .is_none_or(|reload| reload.resident)
                        && instance.observation.is_none()
                })
                .ok_or_else(|| rejected("scope_parent_unavailable"))?;
            let owner = owners
                .get(parent)
                .filter(|owner| owner.parent.is_none() && !owner.cancelled.load(Ordering::Acquire))
                .ok_or_else(|| rejected("scope_parent_unowned"))?;
            owner.process.corroborate(pid)?;
            if let Some(reload) = &instance.reload {
                reload.admit_scope(pid)?;
            }
            let policy_confirmed = instance.reload.is_none();
            if instances.len() >= self.limits.bindings {
                return Err(self.overload("binding_limit"));
            }
            let policy = (
                instance.bytes.clone(),
                instance.host.target().to_owned(),
                instance.host.capabilities().clone().into(),
            );
            instances.insert(id.clone(), None);
            owners.insert(
                id.clone(),
                Owner {
                    parent: Some(parent.into()),
                    acknowledge_by: Some(deadline + Duration::from_secs(5)),
                    policy_confirmed,
                    process,
                    cancelled: Arc::clone(&cancellation),
                },
            );
            policy
        };
        // Host accepts one cancellation flag; propagate connection loss into the
        // same flag used by parent close and process-death maintenance.
        let finished = AtomicBool::new(false);
        let prepared = thread::scope(|scope| {
            scope.spawn(|| {
                while !finished.load(Ordering::Acquire) {
                    if cancelled.load(Ordering::Acquire)
                        || self.stopped.load(Ordering::Acquire)
                        || Instant::now() >= deadline
                    {
                        cancellation.store(true, Ordering::Release);
                        break;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            });
            let result = self.prepare(
                &bytes,
                &id,
                &target,
                capabilities,
                context.clone(),
                (deadline, &cancellation),
            );
            finished.store(true, Ordering::Release);
            result
        });
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let mut owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        let result = prepared.and_then(|host| {
            if cancelled.load(Ordering::Acquire)
                || cancellation.load(Ordering::Acquire)
                || self.stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
                || !instances
                    .get(parent)
                    .and_then(Option::as_ref)
                    .is_some_and(|instance| !instance.closing)
            {
                return Err(rejected("scope_cancelled"));
            }
            owners
                .get(&id)
                .ok_or_else(|| rejected("scope_cancelled"))?
                .process
                .check()?;
            let root = owners
                .get(parent)
                .ok_or_else(|| rejected("scope_cancelled"))?;
            if root.cancelled.load(Ordering::Acquire) {
                return Err(rejected("scope_cancelled"));
            }
            root.process.check()?;
            self.lifecycle_record(
                "task_scope",
                &id,
                json!({"parent_instance_id": parent, "process": owners[&id].process.metadata(), "policy_revision": host.revision()}),
            )?;
            self.contexts.activate(&id)?;
            Ok(host)
        });
        match result {
            Ok(host) => {
                owners
                    .get_mut(&id)
                    .ok_or_else(|| rejected("scope_cancelled"))?
                    .acknowledge_by = Some(Instant::now() + Duration::from_secs(5));
                instances.insert(id.clone(), Some(Instance::prepared(bytes, context, host)));
                Ok(json!(Binding {
                    identity: self.identity.clone(),
                    instance_id: id.clone(),
                    target,
                    audit_key: id
                }))
            }
            Err(error) => {
                instances.remove(&id);
                owners.remove(&id);
                Err(error)
            }
        }
    }

    pub(super) fn drain_scope_children(&self, id: &str, deadline: Instant) -> Result<()> {
        loop {
            let children = {
                let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
                if let Some(owner) = owners.get(id) {
                    owner.cancelled.store(true, Ordering::Release);
                }
                owners
                    .iter()
                    .filter(|(_, owner)| owner.parent.as_deref() == Some(id))
                    .map(|(key, owner)| {
                        owner.cancelled.store(true, Ordering::Release);
                        key.clone()
                    })
                    .collect::<Vec<_>>()
            };
            if children.is_empty() {
                return Ok(());
            }
            // Cancel every sibling before waiting for any one Provider to terminate.
            {
                let events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
                for entry in events
                    .values()
                    .filter(|entry| children.contains(&entry.instance_id))
                {
                    entry.cancelled.store(true, Ordering::Release);
                }
            }
            for child in children {
                let ready = self
                    .instances
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .get(&child)
                    .is_some_and(Option::is_some);
                if ready {
                    match self.release_instance(&child, deadline) {
                        Err(Error::Rejected(code)) if code == "unknown_instance" => {}
                        result => {
                            result?;
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(rejected("instance_release_timeout"));
            }
            thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(2)),
            );
        }
    }

    pub(super) fn maintain_scopes(&self) -> Result<()> {
        let expired = {
            let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
            owners
                .iter()
                .filter(|(_, owner)| {
                    owner.cancelled.load(Ordering::Acquire)
                        || owner.process.check().is_err()
                        || owner
                            .acknowledge_by
                            .is_some_and(|deadline| Instant::now() >= deadline)
                })
                .map(|(id, owner)| {
                    owner.cancelled.store(true, Ordering::Release);
                    id.clone()
                })
                .collect::<Vec<_>>()
        };
        for id in expired {
            // Preparation owns pending reservations and observes the cancellation flag.
            let ready = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .get(&id)
                .is_some_and(Option::is_some);
            if ready {
                match self.release_instance(&id, Instant::now() + Duration::from_millis(5)) {
                    Err(Error::Rejected(code))
                        if matches!(
                            code.as_str(),
                            "unknown_instance" | "instance_release_timeout"
                        ) => {}
                    result => {
                        result?;
                    }
                }
            }
        }
        Ok(())
    }
}
