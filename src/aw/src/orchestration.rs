//! Ordered Core plans and independent OS protection admission.
//!
//! The caller authenticates Core journals and the OS authority, pins the plan
//! for a boundary event, and serializes final admission with native dispatch.
//! These pure checks neither schedule providers nor install an OS policy.

use crate::{
    canonical, require,
    validation::{array, number, profile, same, string, AdoptionCheck},
    Error, Registry,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One settled provider invocation and its separately delivered output.
pub struct InvocationEvidence<'a> {
    /// Invocation admitted against trusted provider and runtime descriptors.
    pub invocation: &'a Value,
    /// Host-owned receipt for that invocation.
    pub receipt: &'a Value,
    /// Output document when the receipt references one.
    pub output: Option<&'a Value>,
}

/// Required inputs for a final pre-tool admission check.
pub struct DispatchCheck<'a> {
    /// Trusted, pinned Core plan for the native boundary event.
    pub plan: &'a Value,
    /// Core-owned terminal execution journal for the whole plan.
    pub execution: &'a Value,
    /// All and only the invocations referenced by the journal.
    pub invocations: &'a [InvocationEvidence<'a>],
    /// Trusted native boundary descriptor.
    pub boundary: &'a Value,
    /// Original complete intent inspected by the command providers.
    pub intent: &'a Value,
    /// Intent captured again by the final native executor.
    pub current_intent: &'a Value,
    /// Current binding independently obtained from the OS policy authority.
    pub protection: &'a Value,
    /// Authenticated authority expected to own this protection binding.
    pub protection_authority: &'a str,
    /// Trusted current Unix epoch milliseconds.
    pub now_ms: u64,
}

fn source_digest(invocation: &Value) -> &Value {
    if invocation["capability"] == "security.command.inspect/v2" {
        &invocation["input"]["command"]["digest"]
    } else {
        &invocation["input"]["artifact"]["digest"]
    }
}

impl Registry {
    /// Validates the ordered plan against a trusted native boundary.
    ///
    /// Every step uses the unchanged boundary source in this profile. Candidate
    /// rescanning, arbitrary DAGs and retries require separate reviewed profiles.
    ///
    /// # Errors
    /// Rejects duplicate identities, incompatible routing, unsafe failure rules
    /// and pre-tool plans lacking mandatory command checks or final guarding.
    pub fn validate_plan(&self, plan: &Value, boundary: &Value) -> Result<(), Error> {
        self.validate("capability-plan-v1", plan)?;
        self.validate_boundary(boundary)?;
        require(
            plan["boundary_id"] == boundary["boundary_id"]
                && plan["boundary_revision"] == boundary["revision"]
                && plan["boundary"] == boundary["boundary"],
            "plan boundary mismatch",
        )?;
        let pre = plan["boundary"] == "pre_tool";
        let mut ids = BTreeSet::new();
        let mut command_gate = false;
        let mut projection_seen = false;
        for step in array(&plan["steps"])? {
            require(!projection_seen, "projection must be the final plan step")?;
            require(ids.insert(string(&step["step_id"])?), "duplicate plan step")?;
            let name = profile(&step["capability"])?;
            for direction in ["input", "output"] {
                require(
                    step[format!("{direction}_schema")]
                        == self.reference(&format!("{name}-{direction}-v2"))?,
                    "plan schema mismatch",
                )?;
            }
            let mut providers = BTreeSet::new();
            for provider in array(&step["providers"])? {
                require(
                    providers.insert(string(&provider["provider_id"])?),
                    "duplicate selected provider",
                )?;
            }
            require(
                step["selection"] != "exactly_one" || providers.len() <= 1,
                "single-provider step has multiple targets",
            )?;
            require(
                step["required"] != true || step["on_failure"] != "record_gap_and_continue",
                "required step cannot ignore failure",
            )?;
            require(
                step["on_failure"] != "deny_dispatch" || pre,
                "dispatch denial requires pre-tool plan",
            )?;
            if name == "security-command-inspect" {
                require(
                    pre && step["required"] == true && step["on_failure"] == "deny_dispatch",
                    "command gate must be mandatory and deny on failure",
                )?;
                command_gate = true;
            } else if name == "context-projection-prepare" {
                projection_seen = true;
                require(
                    !pre && boundary["can_replace_text"] == true
                        && step["selection"] == "exactly_one"
                        && step["on_failure"] == "reject_plan",
                    "projection requires a single replaceable candidate with visible fallback",
                )?;
            } else {
                require(
                    matches!(string(&plan["boundary"])?, "pre_tool" | "post_tool"),
                    "inspection requires a tool boundary",
                )?;
            }
        }
        if pre {
            require(
                command_gate
                    && boundary["can_deny_dispatch"] == true
                    && boundary["composition"]["gate"] == "required_final_guard",
                "pre-tool plan requires a non-bypassable command gate",
            )?;
        }
        Ok(())
    }

    /// Correlates a single invocation with its pinned plan and selected step.
    ///
    /// Also call `validate_invocation` for provider, runtime and deadline
    /// admission before starting the provider. Neither check performs the call.
    ///
    /// # Errors
    /// Rejects unplanned calls, source substitution and changed plan identities.
    pub fn validate_plan_invocation(&self, plan: &Value, invocation: &Value) -> Result<(), Error> {
        self.validate("capability-plan-v1", plan)?;
        self.validate_input(invocation)?;
        same(
            plan,
            invocation,
            &[
                "scope",
                "boundary_id",
                "boundary_revision",
                "policy_revision",
            ],
        )?;
        require(
            plan["boundary"] == invocation["input"]["boundary"]
                && plan["source_digest"] == *source_digest(invocation),
            "plan source or boundary mismatch",
        )?;
        let reference = &invocation["plan_ref"];
        same(plan, reference, &["plan_id", "revision"])?;
        require(
            reference["digest"] == canonical::document_digest(plan)?,
            "plan digest mismatch",
        )?;
        let step = array(&plan["steps"])?
            .iter()
            .find(|s| s["step_id"] == reference["step_id"])
            .ok_or(Error::Invariant("unplanned step"))?;
        same(
            step,
            invocation,
            &["capability", "input_schema", "output_schema"],
        )?;
        require(
            array(&step["providers"])?.iter().any(|p| {
                ["provider_id", "provider_version", "manifest_digest"]
                    .iter()
                    .all(|f| p[*f] == invocation[*f])
            }),
            "unselected provider",
        )
    }

    /// Checks terminal step coverage, Core sequence order and decision reduction.
    ///
    /// Required steps run serially and each selected provider is invoked once.
    /// A terminal denial, fallback or cancellation leaves explicit skipped
    /// entries. Core sequences establish journal order, not cross-host clock
    /// order; their authenticity and append atomicity are caller obligations.
    ///
    /// # Errors
    /// Rejects omitted or reordered steps, duplicate/unbound receipts, fabricated
    /// success, skipped required work and attempts to turn denial into proceed.
    pub fn validate_plan_execution(
        &self,
        plan: &Value,
        execution: &Value,
        invocations: &[InvocationEvidence<'_>],
        boundary: &Value,
    ) -> Result<(), Error> {
        self.validate_plan(plan, boundary)?;
        self.validate("plan-execution-v1", execution)?;
        same(
            plan,
            execution,
            &[
                "plan_id",
                "revision",
                "event_id",
                "scope",
                "boundary_id",
                "boundary_revision",
            ],
        )?;
        require(
            execution["plan_digest"] == canonical::document_digest(plan)?,
            "execution plan digest mismatch",
        )?;
        let steps = array(&plan["steps"])?;
        let entries = array(&execution["steps"])?;
        require(
            steps.len() == entries.len(),
            "execution must account for every step",
        )?;
        let mut available = BTreeMap::new();
        let mut request_keys = BTreeSet::new();
        for evidence in invocations {
            self.validate_plan_invocation(plan, evidence.invocation)?;
            self.validate_result(evidence.invocation, evidence.receipt, evidence.output)?;
            require(
                request_keys.insert((
                    string(&evidence.invocation["provider_id"])?,
                    string(&evidence.invocation["idempotency_key"])?,
                )),
                "provider idempotency key reused within plan",
            )?;
            let id = string(&evidence.invocation["invocation_id"])?;
            require(
                available.insert(id, evidence).is_none(),
                "duplicate invocation evidence",
            )?;
        }
        let mut used = BTreeSet::new();
        let mut previous_settled = None;
        let mut decision = "proceed";
        for (step, entry) in steps.iter().zip(entries) {
            same(step, entry, &["step_id"])?;
            if decision != "proceed" {
                require(
                    entry["outcome"] == "skipped" && entry["reason"] == "previous_step_stopped",
                    "stopped plan must skip remaining steps",
                )?;
                continue;
            }
            require(
                entry["outcome"] != "skipped",
                "step skipped before terminal decision",
            )?;
            let started = number(&entry["started_sequence"])?;
            let settled = number(&entry["settled_sequence"])?;
            require(
                started < settled && previous_settled.is_none_or(|last| last < started),
                "step execution order mismatch",
            )?;
            previous_settled = Some(settled);
            let mut providers = BTreeSet::new();
            let mut gap = false;
            let mut rejected = false;
            for reference in array(&entry["invocations"])? {
                let id = string(&reference["invocation_id"])?;
                require(used.insert(id), "invocation reused across steps")?;
                let evidence = available
                    .get(id)
                    .ok_or(Error::Invariant("missing invocation evidence"))?;
                require(
                    evidence.invocation["plan_ref"]["step_id"] == step["step_id"]
                        && reference["receipt_digest"]
                            == canonical::document_digest(evidence.receipt)?,
                    "step receipt mismatch",
                )?;
                require(
                    providers.insert(string(&evidence.invocation["provider_id"])?),
                    "provider invoked more than once",
                )?;
                gap |= evidence.receipt["disposition"] != "produced";
                if step["capability"] == "security.command.inspect/v2" {
                    rejected |= evidence
                        .output
                        .is_some_and(|out| out["decision"]["verdict"] != "allow");
                }
            }
            if entry["outcome"] == "cancelled" {
                decision = "cancelled";
                continue;
            }
            let selected = array(&step["providers"])?;
            require(
                providers.len() == selected.len(),
                "selected providers not fully accounted for",
            )?;
            gap |= selected.is_empty();
            require(
                (entry["outcome"] == "gap") == gap,
                "step outcome contradicts receipts",
            )?;
            if rejected {
                decision = "deny";
            } else if gap {
                decision = match string(&step["on_failure"])? {
                    "record_gap_and_continue" => "proceed",
                    "deny_dispatch" => "deny",
                    "reject_plan" if plan["boundary"] == "pre_tool" => "deny",
                    "reject_plan" => "preserve",
                    _ => return Err(Error::Invariant("unknown failure policy")),
                };
            }
        }
        require(
            used.len() == available.len(),
            "unreferenced invocation evidence",
        )?;
        require(
            execution["decision"] == decision,
            "plan decision contradicts executed steps",
        )
    }

    /// Checks projection adoption only after accounting for the complete plan.
    ///
    /// # Errors
    /// Rejects adoption after plan fallback/cancellation and observations whose
    /// invocation evidence was not part of the executed plan. The native caller
    /// still authenticates the final observation and ledger acknowledgement.
    pub fn validate_plan_adoption(
        &self,
        plan: &Value,
        execution: &Value,
        invocations: &[InvocationEvidence<'_>],
        observation: AdoptionCheck<'_>,
    ) -> Result<Option<u64>, Error> {
        self.validate_plan_execution(plan, execution, invocations, observation.boundary)?;
        require(
            plan["boundary"] != "pre_tool",
            "projection adoption requires a result boundary",
        )?;
        require(
            invocations.iter().any(|e| {
                e.invocation == observation.invocation
                    && e.receipt == observation.receipt
                    && e.output == observation.output
            }),
            "adoption evidence is not in executed plan",
        )?;
        require(
            observation.observation["decision"] != "adopted" || execution["decision"] == "proceed",
            "stopped plan cannot adopt candidate",
        )?;
        self.validate_adoption(observation)
    }

    /// Checks independent OS coverage for an intent's target and policy.
    ///
    /// The caller authenticates the authority and obtains fresh state directly
    /// from it. An adapter/provider assertion of `enforced` is not sufficient.
    ///
    /// # Errors
    /// Rejects unavailable, expired, stale, partial or merely declared protection.
    /// This checks supplied evidence; it neither probes the OS nor installs rules.
    pub fn validate_os_protection(
        &self,
        protection: &Value,
        intent: &Value,
        plan: &Value,
        authority: &str,
        now_ms: u64,
    ) -> Result<(), Error> {
        self.validate("os-protection-binding-v1", protection)?;
        self.validate("execution-intent-v1", intent)?;
        self.validate("capability-plan-v1", plan)?;
        require(
            plan["boundary"] == "pre_tool",
            "OS requirement needs a pre-tool plan",
        )?;
        same(plan, intent, &["scope"])?;
        let requirement = &plan["os_requirement"];
        same(
            protection,
            intent,
            &["scope", "target_id", "target_generation"],
        )?;
        require(
            protection["authority_id"] == authority && protection["state"] == "active",
            "OS authority or active state mismatch",
        )?;
        require(
            protection["policy_digest"] == intent["protection_policy_digest"]
                && protection["policy_digest"] == requirement["policy_digest"],
            "OS policy mismatch",
        )?;
        require(
            number(&protection["observed_at_ms"])? <= now_ms
                && now_ms < number(&protection["expires_at_ms"])?,
            "OS binding stale or from future",
        )?;
        let required = array(&requirement["required_controls"])?;
        require(!required.is_empty(), "OS requirements cannot be empty")?;
        let mut controls = BTreeMap::new();
        for control in array(&protection["controls"])? {
            require(
                controls
                    .insert(string(&control["control_id"])?, control)
                    .is_none(),
                "duplicate OS control",
            )?;
        }
        for control in required {
            require(
                controls
                    .get(string(control)?)
                    .is_some_and(|c| c["coverage"] == "enforced"),
                "required OS control is not enforced",
            )?;
        }
        Ok(())
    }

    /// Admits dispatch only after the whole plan and independent OS checks pass.
    ///
    /// # Errors
    /// Rejects any non-proceed plan, missing mandatory inspection, changed intent
    /// or inadequate OS protection. The executor must atomically validate fresh
    /// trusted inputs and dispatch; returning `Ok` is not an execution permit.
    pub fn validate_dispatch(&self, check: DispatchCheck<'_>) -> Result<(), Error> {
        let DispatchCheck {
            plan,
            execution,
            invocations,
            boundary,
            intent,
            current_intent,
            protection,
            protection_authority,
            now_ms,
        } = check;
        self.validate_plan_execution(plan, execution, invocations, boundary)?;
        require(
            plan["boundary"] == "pre_tool" && execution["decision"] == "proceed",
            "plan does not admit dispatch",
        )?;
        for evidence in invocations {
            if evidence.invocation["capability"] == "security.command.inspect/v2" {
                self.validate_execution_gate(
                    evidence.invocation,
                    evidence
                        .output
                        .ok_or(Error::Invariant("missing command decision"))?,
                    intent,
                    current_intent,
                    now_ms,
                )?;
            }
        }
        self.validate_os_protection(
            protection,
            current_intent,
            plan,
            protection_authority,
            now_ms,
        )
    }
}
