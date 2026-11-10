//! Bind native events to a single admitted reservation without invalidating sibling runs.

use super::*;
use aw_adapter::reload::ExecutionClaim;

#[derive(Clone)]
pub(in crate::runtime) struct Execution {
    claim: ExecutionClaim,
    generation: u64,
}

impl Reload {
    fn execution_generation(&self, claim: &ExecutionClaim) -> Result<u64> {
        let generation = match claim {
            ExecutionClaim::Request(key) => self.requests.get(key),
            ExecutionClaim::Tool(key) => self.tools.get(key),
            ExecutionClaim::SerialRequest if self.requests.len() == 1 => {
                self.requests.values().next()
            }
            ExecutionClaim::SerialRequest => None,
        };
        generation
            .copied()
            .ok_or_else(|| rejected("request_not_admitted"))
    }
}

impl Runtime {
    pub(in crate::runtime) fn event_execution(
        &self,
        instance: &Instance,
        id: &str,
        value: &Value,
    ) -> Result<Option<Execution>> {
        self.execution_admission(instance, id)?;
        if !self.native_root(id)? {
            return Ok(None);
        }
        let claim =
            aw_adapter::reload::execution_claim(&instance.host.capabilities().adapter, value)
                .map_err(|_| rejected("native_reservation_identity"))?;
        let generation = instance
            .reload
            .as_ref()
            .ok_or_else(|| rejected("request_not_admitted"))?
            .execution_generation(&claim)?;
        Ok(Some(Execution { claim, generation }))
    }

    pub(in crate::runtime) fn check_execution_admission(
        &self,
        id: &str,
        execution: Option<&Execution>,
    ) -> Result<()> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instances
            .get(id)
            .and_then(Option::as_ref)
            .filter(|instance| !instance.closing)
            .ok_or_else(|| rejected("unknown_or_draining_instance"))?;
        self.execution_admission(instance, id)?;
        if let Some(execution) = execution {
            let generation = instance
                .reload
                .as_ref()
                .ok_or_else(|| rejected("request_not_admitted"))?
                .execution_generation(&execution.claim)?;
            if generation != execution.generation {
                return Err(rejected("request_not_admitted"));
            }
        }
        Ok(())
    }
}
