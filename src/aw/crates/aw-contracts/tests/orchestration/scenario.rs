use super::common::{fixtures, REGISTRY};
use aw_contracts::{
    canonical,
    orchestration::{DispatchCheck, InvocationEvidence},
};
use serde_json::{json, Value};

pub(super) struct Scenario {
    pub plan: Value,
    pub execution: Value,
    pub boundary: Value,
    pub intent: Value,
    pub protection: Value,
    pub records: Vec<Record>,
}

#[derive(Clone)]
pub(super) struct Record {
    pub invocation: Value,
    pub receipt: Value,
    pub output: Option<Value>,
}

impl Scenario {
    pub fn pre_tool() -> Self {
        Self::new(true)
    }

    pub fn post_tool() -> Self {
        Self::new(false)
    }

    fn new(pre: bool) -> Self {
        let f = fixtures();
        let mut result = Self {
            plan: f["capability-plan-v1"].clone(),
            execution: f["plan-execution-v1"].clone(),
            boundary: f["boundary-descriptor-v1"].clone(),
            intent: f["execution-intent-v1"].clone(),
            protection: f["os-protection-binding-v1"].clone(),
            records: vec![],
        };
        if pre {
            result.plan["boundary"] = json!("pre_tool");
            result.plan["source_digest"] =
                f["security-command-inspect-input-v2"]["command"]["digest"].clone();
            result.plan["os_requirement"] = json!({
                "policy_digest": result.intent["protection_policy_digest"],
                "required_controls": ["filesystem.access/v1"],
            });
            result.boundary["boundary"] = json!("pre_tool");
            result.boundary["can_replace_text"] = json!(false);
            result.boundary["can_deny_dispatch"] = json!(true);
            result.boundary["has_final_input_guard"] = json!(true);
            result.boundary["composition"] = json!({
                "input_finality": "revalidate_at_dispatch",
                "gate": "required_final_guard",
                "result_finality": "final",
            });
        }
        result.plan["steps"] = json!([]);
        result.execution["steps"] = json!([]);
        let profiles = if pre {
            ["security-command-inspect", "security-command-inspect"]
        } else {
            ["security-content-inspect", "context-projection-prepare"]
        };
        for (i, profile) in profiles.into_iter().enumerate() {
            let mut step = f["capability-plan-v1"]["steps"][0].clone();
            let capability = match profile {
                "security-command-inspect" => "security.command.inspect/v2",
                "security-content-inspect" => "security.content.inspect/v2",
                _ => "context.projection.prepare/v2",
            };
            step["step_id"] = json!(format!("step-{i}"));
            step["capability"] = json!(capability);
            step["input_schema"] = REGISTRY.reference(&format!("{profile}-input-v2")).unwrap();
            step["output_schema"] = REGISTRY.reference(&format!("{profile}-output-v2")).unwrap();
            step["on_failure"] = json!(if pre { "deny_dispatch" } else { "reject_plan" });
            let mut inv = f["capability-invocation-v1"].clone();
            inv["invocation_id"] = json!(format!("call-{i}"));
            inv["idempotency_key"] = json!(format!("request-{i}"));
            inv["capability"] = step["capability"].clone();
            inv["input_schema"] = step["input_schema"].clone();
            inv["output_schema"] = step["output_schema"].clone();
            inv["input"] = f[format!("{profile}-input-v2")].clone();
            inv["plan_ref"]["step_id"] = step["step_id"].clone();
            let entry = json!({
                "step_id": step["step_id"],
                "outcome": "completed",
                "started_sequence": i * 2 + 1,
                "settled_sequence": i * 2 + 2,
                "invocations": [],
            });
            result.plan["steps"].as_array_mut().unwrap().push(step);
            result.execution["steps"]
                .as_array_mut()
                .unwrap()
                .push(entry);
            result.records.push(Record {
                invocation: inv,
                receipt: f["provider-receipt-v1"].clone(),
                output: Some(f[format!("{profile}-output-v2")].clone()),
            });
        }
        result.refresh();
        result
    }
    pub fn refresh(&mut self) {
        let plan_digest = canonical::document_digest(&self.plan).unwrap();
        for Record {
            invocation: inv,
            receipt,
            output,
        } in &mut self.records
        {
            inv["input_digest"] = json!(canonical::document_digest(&inv["input"]).unwrap());
            inv["plan_ref"]["digest"] = json!(plan_digest);
            for field in [
                "scope",
                "boundary_id",
                "boundary_revision",
                "policy_revision",
            ] {
                inv[field] = self.plan[field].clone();
            }
            inv["plan_ref"]["plan_id"] = self.plan["plan_id"].clone();
            inv["plan_ref"]["revision"] = self.plan["revision"].clone();
            for field in [
                "invocation_id",
                "provider_id",
                "provider_version",
                "manifest_digest",
                "capability",
                "scope",
                "input_schema",
                "input_digest",
                "plan_ref",
            ] {
                receipt[field] = inv[field].clone();
            }
            if let Some(out) = output {
                receipt["output"] = json!({
                    "schema": inv["output_schema"],
                    "digest": canonical::document_digest(out).unwrap(),
                    "bytes": canonical::bytes(out).unwrap().len(),
                });
            } else {
                receipt.as_object_mut().unwrap().remove("output");
            }
        }
        for field in [
            "plan_id",
            "revision",
            "event_id",
            "scope",
            "boundary_id",
            "boundary_revision",
        ] {
            self.execution[field] = self.plan[field].clone();
        }
        self.execution["plan_digest"] = json!(plan_digest);
        for entry in self.execution["steps"].as_array_mut().unwrap() {
            entry["invocations"] = self
                .records
                .iter()
                .filter(|record| record.invocation["plan_ref"]["step_id"] == entry["step_id"])
                .map(|record| {
                    json!({
                        "invocation_id": record.invocation["invocation_id"],
                        "receipt_digest": canonical::document_digest(&record.receipt).unwrap(),
                    })
                })
                .collect();
        }
    }
    fn evidence(&self) -> Vec<InvocationEvidence<'_>> {
        self.records
            .iter()
            .map(|record| InvocationEvidence {
                invocation: &record.invocation,
                receipt: &record.receipt,
                output: record.output.as_ref(),
            })
            .collect()
    }
    pub fn check(&self) -> Result<(), aw_contracts::Error> {
        REGISTRY.validate_plan_execution(
            &self.plan,
            &self.execution,
            &self.evidence(),
            &self.boundary,
        )
    }
    pub fn dispatch(&self) -> Result<(), aw_contracts::Error> {
        self.dispatch_current(&self.intent, &self.protection, "fixture-os-authority", 1500)
    }
    pub fn dispatch_current(
        &self,
        current: &Value,
        protection: &Value,
        authority: &str,
        now: u64,
    ) -> Result<(), aw_contracts::Error> {
        REGISTRY.validate_dispatch(DispatchCheck {
            plan: &self.plan,
            execution: &self.execution,
            invocations: &self.evidence(),
            boundary: &self.boundary,
            intent: &self.intent,
            current_intent: current,
            protection,
            protection_authority: authority,
            now_ms: now,
        })
    }
    pub fn skip_after_first(&mut self) {
        self.records
            .retain(|record| record.invocation["plan_ref"]["step_id"] == "step-0");
        self.execution["steps"][1] = json!({
            "step_id": "step-1",
            "outcome": "skipped",
            "reason": "previous_step_stopped",
            "invocations": [],
        });
        self.refresh();
    }
}
