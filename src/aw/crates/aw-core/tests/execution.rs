//! Synthetic Host tests exercise public Core boundaries, not deployed providers.

use aw_contracts::{canonical, Registry};
use aw_core::{
    ports::{
        Cancellation, Clock, HostError, Journal, JournalError, NeverCancel, ProviderHost,
        ProviderResult,
    },
    Core, Error, PrepareRequest, StepInput,
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

struct CancelFlag(Rc<Cell<bool>>);
impl Cancellation for CancelFlag {
    fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}

#[derive(Default)]
struct MemoryJournal {
    claims: BTreeSet<String>,
    writers: BTreeSet<String>,
    releases: usize,
    records: Vec<Value>,
    fail_claim: bool,
    fail_append: Option<usize>,
}
impl Journal for MemoryJournal {
    fn claim(&mut self, event_key: &str, plan: &Value) -> Result<Value, JournalError> {
        if self.fail_claim {
            return Err(JournalError::InvalidRecord);
        }
        if !self.claims.insert(event_key.to_owned()) {
            return Err(JournalError::AlreadyClaimed);
        }
        self.writers.insert(event_key.to_owned());
        Ok(ack(plan, "claim"))
    }
    fn append(&mut self, event_key: &str, record: &Value) -> Result<Value, JournalError> {
        if !self.writers.contains(event_key) || self.fail_append == Some(self.records.len()) {
            return Err(JournalError::InvalidRecord);
        }
        self.records.push(record.clone());
        Ok(ack(record, &format!("record-{}", self.records.len())))
    }
    fn release(&mut self, event_key: &str) {
        self.writers.remove(event_key);
        self.releases += 1;
    }
}
fn ack(record: &Value, id: &str) -> Value {
    json!({"source_id":"synthetic-journal", "record_id":id,
        "digest":canonical::document_digest(record).unwrap()})
}

#[derive(Clone, Copy)]
enum Reply {
    Allow,
    Deny,
    Warn,
    Failed,
    Transport,
    WrongReceipt,
    WrongInput,
    WrongOutput,
    LateReceipt,
}
struct Host {
    descriptors: BTreeMap<String, Value>,
    invoked: Vec<Value>,
    replies: Vec<Reply>,
    cancel_after_call: Option<Rc<Cell<bool>>>,
}
impl Host {
    fn new() -> Self {
        Self {
            descriptors: BTreeMap::from([(
                "fixture-provider".into(),
                fixtures()["provider-descriptor-v1"].clone(),
            )]),
            invoked: vec![],
            replies: vec![],
            cancel_after_call: None,
        }
    }
    fn add_provider(&mut self, id: &str) {
        let mut descriptor = fixtures()["provider-descriptor-v1"].clone();
        descriptor["provider_id"] = json!(id);
        self.descriptors.insert(id.into(), descriptor);
    }
}
impl ProviderHost for Host {
    fn descriptor(&self, id: &str) -> Option<&Value> {
        self.descriptors.get(id)
    }
    fn invoke(&mut self, invocation: &Value) -> Result<ProviderResult, HostError> {
        let reply = self
            .replies
            .get(self.invoked.len())
            .copied()
            .unwrap_or(Reply::Allow);
        self.invoked.push(invocation.clone());
        if let Some(flag) = &self.cancel_after_call {
            flag.set(true);
        }
        if matches!(reply, Reply::Transport) {
            return Err(HostError {
                code: "synthetic_transport_failure".into(),
            });
        }
        let f = fixtures();
        let profile = match invocation["capability"].as_str().unwrap() {
            "security.command.inspect/v2" => "security-command-inspect",
            "security.content.inspect/v2" => "security-content-inspect",
            "context.projection.prepare/v2" => "context-projection-prepare",
            other => panic!("unexpected synthetic capability: {other}"),
        };
        let mut output = f[format!("{profile}-output-v2")].clone();
        if matches!(reply, Reply::Deny | Reply::Warn) {
            output["decision"]["verdict"] = json!(if matches!(reply, Reply::Deny) {
                "deny"
            } else {
                "warn"
            });
            output["decision"]["findings"] = json!([{"rule_id":"fixture-rule", "category":"dangerous_pattern", "severity":"high", "confidence":"high", "count":1}]);
            output["decision"]["reasons"] = json!(["policy.synthetic"]);
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
        receipt["output"] = json!({"schema":invocation["output_schema"], "digest":canonical::document_digest(&output).unwrap(), "bytes":canonical::bytes(&output).unwrap().len()});
        match reply {
            Reply::Failed => {
                receipt["disposition"] = json!("failed");
                receipt["error_code"] = json!("synthetic_failure");
                receipt.as_object_mut().unwrap().remove("output");
                return Ok(ProviderResult {
                    receipt,
                    output: None,
                });
            }
            Reply::WrongReceipt => receipt["invocation_id"] = json!("another-call"),
            Reply::WrongInput => receipt["input_digest"] = json!("0".repeat(64)),
            Reply::WrongOutput => output["inspection"]["verdict"] = json!("tampered"),
            Reply::LateReceipt => receipt["completed_at_ms"] = json!(1101),
            _ => {}
        }
        Ok(ProviderResult {
            receipt,
            output: Some(output),
        })
    }
}

fn request(pre: bool) -> PrepareRequest {
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
    let profiles = if pre {
        ["security-command-inspect", "security-command-inspect"]
    } else {
        ["security-content-inspect", "context-projection-prepare"]
    };
    let mut inputs = BTreeMap::new();
    plan["steps"] = json!([]);
    for (i, profile) in profiles.into_iter().enumerate() {
        let mut step = f["capability-plan-v1"]["steps"][0].clone();
        let id = format!("step-{i}");
        step["step_id"] = json!(id);
        step["capability"] = json!(match profile {
            "security-command-inspect" => "security.command.inspect/v2",
            "security-content-inspect" => "security.content.inspect/v2",
            _ => "context.projection.prepare/v2",
        });
        step["input_schema"] = registry.reference(&format!("{profile}-input-v2")).unwrap();
        step["output_schema"] = registry.reference(&format!("{profile}-output-v2")).unwrap();
        step["on_failure"] = json!(if pre { "deny_dispatch" } else { "reject_plan" });
        plan["steps"].as_array_mut().unwrap().push(step);
        inputs.insert(
            id,
            StepInput {
                input: f[format!("{profile}-input-v2")].clone(),
                budget: f["capability-invocation-v1"]["budget"].clone(),
                deadline_at_ms: 2000,
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
fn second_provider(request: &mut PrepareRequest, host: &mut Host) {
    host.add_provider("second-provider");
    let mut selected = request.plan["steps"][0]["providers"][0].clone();
    selected["provider_id"] = json!("second-provider");
    request.plan["steps"][0]["selection"] = json!("all_distinct_providers");
    request.plan["steps"][0]["providers"]
        .as_array_mut()
        .unwrap()
        .push(selected);
}

#[path = "execution/admission.rs"]
mod admission;
#[path = "execution/decisions.rs"]
mod decisions;
#[path = "execution/storage.rs"]
mod storage;
#[path = "execution/timing.rs"]
mod timing;

#[path = "execution/lifecycle.rs"]
mod lifecycle;
