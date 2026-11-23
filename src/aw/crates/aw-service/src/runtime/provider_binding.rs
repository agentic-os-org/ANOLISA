//! Project admitted Service identity into the optional Provider-only binding.

use super::{policy::Execution, Runtime};
use crate::{rejected, Result};
use aw_host::{BindingContext, Host};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

impl Runtime {
    pub(super) fn provider_binding(
        &self,
        host: &Host,
        instance_id: &str,
        event: &Value,
        association: &Value,
        execution: Option<&Execution>,
    ) -> Result<Option<BindingContext>> {
        let agent = &host.configuration().as_value()["spec"]["agents"][host.target()];
        let Some(workspace_id) = agent["workspace_id"].as_str() else {
            return Ok(None);
        };
        let (session_id, task_version) = if let Some(execution) = execution {
            // Native tools must still corroborate the admitted reservation's callback.
            if event["tool"].is_object() && association["status"] != "verified" {
                return Ok(None);
            }
            let Some((native_session, generation)) = event["session_id"]
                .as_str()
                .filter(|id| {
                    !id.trim().is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
                })
                .zip(execution.task_generation())
            else {
                return Ok(None);
            };
            let key = json!(["aw-provider-session/v1", instance_id, native_session]);
            (
                format!("{:x}", Sha256::digest(key.to_string().as_bytes())),
                format!("request-{generation}"),
            )
        } else {
            let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
            let Some(session) = owners
                .get(instance_id)
                .and_then(|owner| owner.provider_session.as_ref())
            else {
                return Ok(None);
            };
            (session.clone(), instance_id.to_owned())
        };
        Ok(Some(BindingContext {
            session_id,
            instance_id: instance_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            task_version,
        }))
    }
}
