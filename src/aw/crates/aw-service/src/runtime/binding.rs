//! Reserve an instance before Provider preparation and release failed reservations.

use super::{Instance, Runtime, MAX_BINDINGS};
use crate::{rejected, Binding, Capabilities, Result};
use aw_host::ProcessContext;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

impl Runtime {
    pub(super) fn bind(
        &self,
        target: String,
        capabilities: Capabilities,
        context: ProcessContext,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let instance_id = self.identifier()?;
        {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            if instances.len() >= MAX_BINDINGS {
                return Err(rejected("binding_limit"));
            }
            instances.insert(instance_id.clone(), None);
        }
        let result = self.prepare(
            &instance_id,
            &target,
            capabilities,
            context,
            deadline,
            cancelled,
        );
        let result = result.and_then(|host| {
            if cancelled.load(Ordering::Acquire)
                || self.stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
            {
                Err(rejected("binding_cancelled"))
            } else {
                Ok(host)
            }
        });
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        match result {
            Ok(host) => {
                self.contexts.activate(&instance_id)?;
                instances.insert(
                    instance_id.clone(),
                    Some(Instance {
                        host: Arc::new(host),
                        closing: false,
                        observation: None,
                        hooks: Arc::new(Mutex::new(BTreeMap::new())),
                        hook_open: Arc::new(Mutex::new(())),
                    }),
                );
                Ok(json!(Binding {
                    identity: self.identity.clone(),
                    instance_id: instance_id.clone(),
                    target,
                    audit_key: instance_id
                }))
            }
            Err(error) => {
                instances.remove(&instance_id);
                Err(error)
            }
        }
    }
}
