//! Reserve an instance before Provider preparation and release failed reservations.

use super::{Instance, Runtime};
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
        self.bind_id(
            self.identifier()?,
            &self.bytes,
            target,
            capabilities,
            context,
            (deadline, cancelled),
        )
    }

    pub(super) fn bind_id(
        &self,
        instance_id: String,
        bytes: &[u8],
        target: String,
        capabilities: Capabilities,
        context: ProcessContext,
        preparation: (Instant, &AtomicBool),
    ) -> Result<Value> {
        let (deadline, cancelled) = preparation;
        {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            if instances.len() >= self.limits.bindings {
                return Err(self.overload("binding_limit"));
            }
            instances.insert(instance_id.clone(), None);
        }
        let result = self.prepare(
            bytes,
            &instance_id,
            &target,
            capabilities,
            context.clone(),
            (deadline, cancelled),
        );
        let result = result.and_then(|host| {
            if cancelled.load(Ordering::Acquire)
                || self.stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
            {
                Err(rejected("binding_cancelled"))
            } else {
                self.contexts.activate(&instance_id)?;
                Ok(host)
            }
        });
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        match result {
            Ok(host) => {
                instances.insert(
                    instance_id.clone(),
                    Some(Instance::prepared(bytes.to_vec(), context, host)),
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

impl Instance {
    pub(super) fn prepared(
        bytes: Vec<u8>,
        context: ProcessContext,
        host: impl Into<Arc<aw_host::Host>>,
    ) -> Self {
        let host = host.into();
        let routes = Self::prepare_routes(&host);
        Self {
            run: None,
            native_facts: Arc::default(),
            bytes,
            context,
            reload: None,
            policy_update: Arc::new(Mutex::new(())),
            release: Arc::new(Mutex::new(())),
            host,
            routes,
            closing: false,
            observation: None,
            hooks: Arc::new(Mutex::new(BTreeMap::new())),
            hook_open: Arc::new(Mutex::new(())),
            invocations: Arc::default(),
        }
    }

    pub(super) fn prepare_routes(
        host: &aw_host::Host,
    ) -> Option<Arc<aw_core::routes::PreparedRoutes>> {
        aw_adapter::reload::scope_routes(
            host.configuration().as_value(),
            host.target(),
            host.steps(),
            host.capabilities(),
        )
        .ok()
        .map(Arc::new)
    }
}

impl Runtime {
    pub(super) fn lifecycle_record(&self, kind: &str, id: &str, metadata: Value) -> Result<String> {
        let key = self.identifier()?;
        self.audit.claim(
            &key,
            json!({"kind":kind,"identity":self.identity,"instance_id":id,"metadata":metadata}),
        )?;
        let result = self
            .audit
            .append(&key, json!({"phase":"closed","status":"ok"}));
        self.audit.release(&key);
        result?;
        Ok(key)
    }
}
