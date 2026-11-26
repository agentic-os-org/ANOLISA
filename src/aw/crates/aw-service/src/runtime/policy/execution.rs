//! Bind native events to a single admitted reservation without invalidating sibling runs.

use super::*;
use aw_adapter::reload::ExecutionClaim;

#[derive(Clone)]
pub(in crate::runtime) struct Execution {
    claim: ExecutionClaim,
    generation: u64,
    task: Option<(String, u64)>,
}

impl Execution {
    pub(in crate::runtime) fn task_generation(&self) -> Option<u64> {
        match self.claim {
            ExecutionClaim::Tool(_) => self.task.as_ref().map(|(_, generation)| *generation),
            _ => Some(self.generation),
        }
    }
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
        if value.get("cwd").is_some() {
            return Err(rejected("event_cwd_reserved"));
        }
        if !self.native_root(id)? {
            return Ok(None);
        }
        let capabilities = instance.host.capabilities();
        let claim = aw_adapter::reload::execution_claim(
            &capabilities.adapter,
            &capabilities.entrypoint,
            value,
        )
        .map_err(|_| rejected("native_reservation_identity"))?;
        let reload = instance
            .reload
            .as_ref()
            .ok_or_else(|| rejected("request_not_admitted"))?;
        let generation = reload.execution_generation(&claim)?;
        // Qoder has separate tool reservations. Its authenticated session
        // selects the enclosing request that supplied the task version.
        let task = match claim {
            ExecutionClaim::Tool(_) => value["session_id"].as_str().and_then(|session| {
                reload
                    .requests
                    .get(session)
                    .map(|generation| (session.to_owned(), *generation))
            }),
            _ => None,
        };
        Ok(Some(Execution {
            claim,
            generation,
            task,
        }))
    }

    pub(in crate::runtime) fn check_execution_admission(
        &self,
        id: &str,
        execution: Option<&Execution>,
    ) -> Result<()> {
        self.check_admitted_execution(id, execution, false)
    }

    pub(in crate::runtime) fn check_provider_admission(
        &self,
        id: &str,
        execution: Option<&Execution>,
    ) -> Result<()> {
        self.check_admitted_execution(id, execution, true)
    }

    fn check_admitted_execution(
        &self,
        id: &str,
        execution: Option<&Execution>,
        check_task: bool,
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
            let reload = instance
                .reload
                .as_ref()
                .ok_or_else(|| rejected("request_not_admitted"))?;
            let generation = reload.execution_generation(&execution.claim)?;
            if generation != execution.generation
                || (check_task
                    && execution
                        .task
                        .as_ref()
                        .is_some_and(|(session, generation)| {
                            reload.requests.get(session) != Some(generation)
                        }))
            {
                return Err(rejected("request_not_admitted"));
            }
        }
        Ok(())
    }
}
