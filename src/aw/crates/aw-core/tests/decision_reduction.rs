//! Regression tests for the AW Core decision reduction.
//!
//! A mandatory command gate's observed denial must stay terminal even when
//! cooperative cancellation overlaps the same Host call: the executor records
//! `deny` (not `cancelled`), and the plan-execution checker rejects a record
//! whose decision was relabelled as a cancellation.

use aw_contracts::{canonical, Registry};
use aw_core::{
    ports::{Cancellation, Clock, HostError, Journal, JournalError, ProviderHost, ProviderResult},
    Core, PrepareRequest, StepInput,
};
use serde_json::{json, Value};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

fn fixtures() -> Value {
    canonical::parse(include_bytes!("../../../tests/fixtures/contracts.json")).unwrap()
}

struct FixedClock(u64);
impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

struct CancelAt {
    flag: Rc<Cell<bool>>,
}
impl Cancellation for CancelAt {
    fn is_cancelled(&self) -> bool {
        self.flag.get()
    }
}

#[derive(Default)]
struct MemoryJournal {
    claims: BTreeSet<String>,
    writers: BTreeSet<String>,
}
impl Journal for MemoryJournal {
    fn claim(&mut self, event_key: &str, plan: &Value) -> Result<Value, JournalError> {
        if !self.claims.insert(event_key.to_owned()) {
            return Err(JournalError::AlreadyClaimed);
        }
        self.writers.insert(event_key.to_owned());
        Ok(ack(plan, "claim"))
    }
    fn append(&mut self, event_key: &str, record: &Value) -> Result<Value, JournalError> {
        if !self.writers.contains(event_key) {
            return Err(JournalError::InvalidRecord);
        }
        Ok(ack(record, "record"))
    }
    fn release(&mut self, event_key: &str) {
        self.writers.remove(event_key);
    }
}
fn ack(record: &Value, id: &str) -> Value {
    json!({"source_id":"synthetic-journal", "record_id":id,
        "digest":canonical::document_digest(record).unwrap()})
}

struct Host {
    descriptors: BTreeMap<String, Value>,
    replies: Vec<&'static str>,
    cancel_after: Option<(Rc<Cell<bool>>, usize)>,
    invoked: usize,
}
impl ProviderHost for Host {
    fn descriptor(&self, id: &str) -> Option<&Value> {
        self.descriptors.get(id)
    }
    fn invoke(&mut self, invocation: &Value) -> Result<ProviderResult, HostError> {
        let index = self.invoked;
        self.invoked += 1;
        if let Some((flag, at)) = &self.cancel_after {
            if index + 1 >= *at {
                flag.set(true);
            }
        }
        let reply = self.replies.get(index).copied().unwrap_or("allow");
        let f = fixtures();
        let profile = match invocation["capability"].as_str().unwrap() {
            "security.command.inspect/v2" => "security-command-inspect",
            "security.content.inspect/v2" => "security-content-inspect",
            "context.projection.prepare/v2" => "context-projection-prepare",
            other => panic!("unexpected capability {other}"),
        };
        let mut output = f[format!("{profile}-output-v2")].clone();
        if reply == "deny" || reply == "warn" {
            if profile == "security-command-inspect" {
                output["decision"]["verdict"] =
                    json!(if reply == "deny" { "deny" } else { "warn" });
                output["decision"]["findings"] = json!([{"rule_id":"fixture-rule","category":"dangerous_pattern","severity":"high","confidence":"high","count":1}]);
                output["decision"]["reasons"] = json!(["policy.synthetic"]);
            } else {
                output["inspection"]["verdict"] = json!("suspicious");
                output["inspection"]["findings"] = json!([{"rule_id":"fixture-rule","category":"dangerous_pattern","severity":"high","confidence":"high","count":1}]);
            }
        }
        let mut receipt = f["provider-receipt-v1"].clone();
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
            receipt[field] = invocation[field].clone();
        }
        receipt["started_at_ms"] = json!(1100);
        receipt["completed_at_ms"] = json!(1100);
        receipt.as_object_mut().unwrap().remove("output");
        if reply == "failed" {
            receipt["disposition"] = json!("failed");
            receipt["error_code"] = json!("synthetic_failure");
            return Ok(ProviderResult {
                receipt,
                output: None,
            });
        }
        if reply == "bypassed" {
            receipt["disposition"] = json!("bypassed");
            return Ok(ProviderResult {
                receipt,
                output: None,
            });
        }
        receipt["output"] = json!({"schema":invocation["output_schema"], "digest":canonical::document_digest(&output).unwrap(), "bytes":canonical::bytes(&output).unwrap().len()});
        Ok(ProviderResult {
            receipt,
            output: Some(output),
        })
    }
}

fn build(pre: bool, capabilities: &[&str], providers_per_step: usize) -> PrepareRequest {
    let f = fixtures();
    let registry = Registry::new().unwrap();
    let mut plan = f["capability-plan-v1"].clone();
    let mut boundary = f["boundary-descriptor-v1"].clone();
    if pre {
        plan["boundary"] = json!("pre_tool");
        plan["source_digest"] = f["security-command-inspect-input-v2"]["command"]["digest"].clone();
        plan["os_requirement"] = json!({"policy_digest":f["execution-intent-v1"]["protection_policy_digest"], "required_controls":["filesystem.access/v1"]});
        boundary["boundary"] = json!("pre_tool");
        boundary["can_replace_text"] = json!(false);
        boundary["can_deny_dispatch"] = json!(true);
        boundary["has_final_input_guard"] = json!(true);
        boundary["composition"] = json!({"input_finality":"revalidate_at_dispatch", "gate":"required_final_guard", "result_finality":"final"});
    }
    let mut inputs = BTreeMap::new();
    plan["steps"] = json!([]);
    for (i, capability) in capabilities.iter().enumerate() {
        let profile = match *capability {
            "security.command.inspect/v2" => "security-command-inspect",
            "security.content.inspect/v2" => "security-content-inspect",
            other => panic!("unexpected {other}"),
        };
        let mut step = f["capability-plan-v1"]["steps"][0].clone();
        let id = format!("step-{i}");
        step["step_id"] = json!(id);
        step["capability"] = json!(capability);
        step["input_schema"] = registry.reference(&format!("{profile}-input-v2")).unwrap();
        step["output_schema"] = registry.reference(&format!("{profile}-output-v2")).unwrap();
        step["on_failure"] = json!(if pre { "deny_dispatch" } else { "reject_plan" });
        step["required"] = json!(true);
        let mut selected = vec![f["capability-plan-v1"]["steps"][0]["providers"][0].clone()];
        for extra in 1..providers_per_step {
            let mut provider = selected[0].clone();
            provider["provider_id"] = json!(format!("provider-{extra}"));
            selected.push(provider);
        }
        step["providers"] = json!(selected);
        step["selection"] = json!(if providers_per_step == 1 {
            "exactly_one"
        } else {
            "all_distinct_providers"
        });
        plan["steps"].as_array_mut().unwrap().push(step);
        inputs.insert(
            id,
            StepInput {
                input: f[format!("{profile}-input-v2")].clone(),
                budget: f["capability-invocation-v1"]["budget"].clone(),
                deadline_at_ms: 5000,
            },
        );
    }
    PrepareRequest {
        plan,
        boundary,
        runtime: f["runtime-binding-v1"].clone(),
        inputs,
    }
}

fn host(providers: usize) -> Host {
    let f = fixtures();
    let mut descriptors = BTreeMap::new();
    for i in 0..providers {
        let mut descriptor = f["provider-descriptor-v1"].clone();
        descriptor["provider_id"] = json!(if i == 0 {
            "fixture-provider".to_string()
        } else {
            format!("provider-{i}")
        });
        descriptors.insert(
            descriptor["provider_id"].as_str().unwrap().to_string(),
            descriptor,
        );
    }
    Host {
        descriptors,
        replies: vec![],
        cancel_after: None,
        invoked: 0,
    }
}

fn prepared_boundary(_: &aw_core::PreparedPlan) -> Value {
    let f = fixtures();
    let mut boundary = f["boundary-descriptor-v1"].clone();
    boundary["boundary"] = json!("pre_tool");
    boundary["can_replace_text"] = json!(false);
    boundary["can_deny_dispatch"] = json!(true);
    boundary["has_final_input_guard"] = json!(true);
    boundary["composition"] = json!({"input_finality":"revalidate_at_dispatch", "gate":"required_final_guard", "result_finality":"final"});
    boundary
}

#[test]
fn deny_survives_cancellation_overlapping_the_call() {
    let core = Core::new().unwrap();
    let request = build(true, &["security.command.inspect/v2"], 1);
    let mut h = host(1);
    h.replies = vec!["deny"];
    let flag = Rc::new(Cell::new(false));
    h.cancel_after = Some((flag.clone(), 1));
    let prepared = core.prepare(request, &h, 1000).unwrap();
    let execution = core
        .execute(
            prepared,
            &mut h,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &CancelAt { flag },
        )
        .unwrap();
    let verdict =
        execution.calls()[0].result().output.as_ref().unwrap()["decision"]["verdict"].clone();
    println!(
        "PROBE deny+cancel: recorded decision={} observed verdict={}",
        execution.record()["decision"],
        verdict
    );
    assert_eq!(verdict, "deny");
    assert_eq!(execution.record()["decision"], "deny");
}

#[test]
fn checker_rejects_deny_relabelled_as_cancelled() {
    use aw_contracts::Registry;
    let core = Core::new().unwrap();
    let request = build(true, &["security.command.inspect/v2"], 1);
    let mut h = host(1);
    h.replies = vec!["deny"];
    let flag = Rc::new(Cell::new(false));
    h.cancel_after = Some((flag.clone(), 1));
    let prepared = core.prepare(request, &h, 1000).unwrap();
    let boundary = prepared_boundary(&prepared);
    let plan = prepared.plan().clone();
    let execution = core
        .execute(
            prepared,
            &mut h,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &CancelAt { flag },
        )
        .unwrap();
    let record = execution.record().clone();
    let evidence: Vec<_> = execution.calls().iter().map(|c| c.evidence()).collect();
    let owned: Vec<_> = evidence
        .iter()
        .map(|e| aw_contracts::orchestration::InvocationEvidence {
            invocation: e.invocation,
            receipt: e.receipt,
            output: e.output,
        })
        .collect();
    let registry = Registry::new().unwrap();

    // The executor must record the gate's denial, not a cancellation.
    assert_eq!(record["decision"], "deny");

    // The untouched record validates ...
    let checked = registry.validate_plan_execution(&plan, &record, &owned, &boundary);
    assert!(checked.is_ok(), "deny record rejected: {checked:?}");

    // ... while a relabelled denial (the pre-fix output) must not.
    let mut relabelled = record.clone();
    relabelled["decision"] = serde_json::json!("cancelled");
    let checked = registry.validate_plan_execution(&plan, &relabelled, &owned, &boundary);
    assert!(
        checked.is_err(),
        "checker accepted a denial relabelled as cancelled: {checked:?}"
    );
}
