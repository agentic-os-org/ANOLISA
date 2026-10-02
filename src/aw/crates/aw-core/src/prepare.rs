//! Resolve all selected providers before any capability is invoked.

use crate::{ports::ProviderHost, Core, Error};
use aw_contracts::canonical;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// One unchanged boundary input and its per-provider limits.
pub struct StepInput {
    /// Exact capability input using the plan's schema revision.
    pub input: Value,
    /// The existing invocation budget object, enforced by the Host.
    pub budget: Value,
    /// Absolute Unix epoch millisecond deadline, never extended during execution.
    pub deadline_at_ms: u64,
}

/// Trusted native context and policy-resolved plan supplied to Core.
///
/// The caller authenticates runtime/boundary facts and policy selection. This
/// is a Rust embedding API, not a new generic Agent message or public wire schema.
/// Assign a distinct `event_id` to each native boundary occurrence: pre-tool and
/// post-tool occurrences of one tool call have different IDs. Retries of the same
/// occurrence retain its ID so journal reservation prevents duplicate execution.
pub struct PrepareRequest {
    /// Ordered capability plan with exact provider/schema selections.
    pub plan: Value,
    /// Current, authenticated native extension powers.
    pub boundary: Value,
    /// Current binding obtained from the original runtime owner.
    pub runtime: Value,
    /// Exactly one input per plan step, keyed by step_id.
    pub inputs: BTreeMap<String, StepInput>,
}

pub(crate) struct PlannedCall {
    pub(crate) invocation: Value,
    pub(crate) provider: Value,
}

pub(crate) struct PlannedStep {
    pub(crate) step: Value,
    pub(crate) calls: Vec<PlannedCall>,
}

/// Immutable, fully admitted plan. Execution consumes this value once.
///
/// Journal reservation additionally rejects duplicate preparations of the same
/// scoped native event across executions or restarts. Preparation is not a permit.
pub struct PreparedPlan {
    pub(crate) plan: Value,
    pub(crate) boundary: Value,
    pub(crate) runtime: Value,
    pub(crate) event_key: String,
    pub(crate) steps: Vec<PlannedStep>,
}

impl PreparedPlan {
    /// The policy-resolved plan; callers cannot mutate it after admission.
    pub fn plan(&self) -> &Value {
        &self.plan
    }

    /// Stable scope/event key used for journal lookup and duplicate rejection.
    ///
    /// Distinct boundary occurrences require distinct `event_id` values in the
    /// request; changing a plan or retrying an occurrence must retain its event ID.
    pub fn event_key(&self) -> &str {
        &self.event_key
    }
}

impl Core {
    /// Pins a whole plan and admits every planned call before executing any.
    ///
    /// Empty selected routes remain explicit gaps. Missing named providers,
    /// changed schemas and incompatible boundaries fail preparation as a whole.
    ///
    /// # Errors
    /// Rejects invalid plans, missing/extra inputs, unsupported providers,
    /// mismatched bindings and expired per-call deadlines.
    pub fn prepare(
        &self,
        request: PrepareRequest,
        host: &impl ProviderHost,
        now_ms: u64,
    ) -> Result<PreparedPlan, Error> {
        let PrepareRequest {
            plan,
            boundary,
            runtime,
            mut inputs,
        } = request;
        self.registry.validate_plan(&plan, &boundary)?;
        self.registry.validate("runtime-binding-v1", &runtime)?;
        // Empty routes still belong to a real current runtime, even though they
        // have no invocation through which validate_invocation can check it.
        for field in ["runtime_id", "binding_revision", "environment_id"] {
            if plan["scope"][field] != runtime[field] {
                return Err(Error::Preparation("runtime scope mismatch"));
            }
        }
        if plan["scope"]["runtime_generation"] != runtime["generation"]
            || runtime["state"] != "running"
            || (runtime.get("session_id").is_some()
                && plan["scope"]["session_id"] != runtime["session_id"])
        {
            return Err(Error::Preparation("runtime binding is not current"));
        }
        let digest = canonical::document_digest(&plan)?;
        let event_key = canonical::document_digest(&json!({
            "scope": plan["scope"], "event_id": plan["event_id"]
        }))?;
        let mut steps = Vec::new();
        for step in plan["steps"]
            .as_array()
            .ok_or(Error::Preparation("missing steps"))?
        {
            let id = step["step_id"]
                .as_str()
                .ok_or(Error::Preparation("missing step ID"))?;
            let input = inputs
                .remove(id)
                .ok_or(Error::Preparation("missing step input"))?;
            let mut calls = Vec::new();
            for selected in step["providers"]
                .as_array()
                .ok_or(Error::Preparation("missing selected providers"))?
            {
                let provider_id = selected["provider_id"]
                    .as_str()
                    .ok_or(Error::Preparation("missing provider ID"))?;
                let provider = host
                    .descriptor(provider_id)
                    .ok_or(Error::ProviderUnavailable)?
                    .clone();
                let key = canonical::document_digest(&json!({
                    "plan_digest": digest, "step_id": id, "provider": selected
                }))?;
                let invocation = json!({
                    "invocation_id": format!("call-{key}"),
                    "idempotency_key": format!("request-{key}"),
                    "provider_id": selected["provider_id"],
                    "provider_version": selected["provider_version"],
                    "manifest_digest": selected["manifest_digest"],
                    "capability": step["capability"],
                    "scope": plan["scope"],
                    "boundary_id": plan["boundary_id"],
                    "boundary_revision": plan["boundary_revision"],
                    "policy_revision": plan["policy_revision"],
                    "deadline_at_ms": input.deadline_at_ms,
                    "budget": input.budget,
                    "input_schema": step["input_schema"],
                    "output_schema": step["output_schema"],
                    "input_digest": canonical::document_digest(&input.input)?,
                    "input": input.input,
                    "plan_ref": {
                        "plan_id": plan["plan_id"], "revision": plan["revision"],
                        "digest": digest, "step_id": id
                    }
                });
                self.registry.validate_plan_invocation(&plan, &invocation)?;
                self.registry.validate_invocation(
                    &invocation,
                    &provider,
                    &boundary,
                    &runtime,
                    now_ms,
                )?;
                calls.push(PlannedCall {
                    invocation,
                    provider,
                });
            }
            steps.push(PlannedStep {
                step: step.clone(),
                calls,
            });
        }
        if !inputs.is_empty() {
            return Err(Error::Preparation("unplanned step input"));
        }
        Ok(PreparedPlan {
            plan,
            boundary,
            runtime,
            event_key,
            steps,
        })
    }
}
