//! Exercise a local Provider through a synthetic Adapter boundary, without an Agent.

use aw_host::{FailureAction, Host, ProcessContext};
use aw_provider::admission::AdapterCapabilities;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(3..=4).contains(&args.len()) || !matches!(args[1].as_str(), "tool.before" | "tool.after") {
        return Err("usage: host CONFIG tool.before|tool.after TOOL_NAME [INPUT_JSON]".into());
    }
    let input = match args.get(3) {
        Some(input) => serde_json::from_str(input)?,
        None => json!({"path": "example.txt"}),
    };
    let bytes = std::fs::read(&args[0])?;
    let cancelled = AtomicBool::new(false);
    // Synthetic capabilities exercise composition; they certify no Agent version.
    let capabilities = AdapterCapabilities {
        adapter: "qoder".into(),
        version: "host-example".into(),
        entrypoint: "synthetic".into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
        ]),
    };
    let host = Host::prepare(
        &bytes,
        "qoder",
        capabilities,
        ProcessContext {
            cwd: std::env::current_dir()?,
            environment: BTreeMap::new(),
            stderr_bytes: 65536,
        },
        Instant::now() + Duration::from_secs(10),
        &cancelled,
    )?;
    let event = host.event(json!({
        "name": args[1], "agent": {"adapter": "qoder", "binding_id": "qoder", "instance_id": null},
        "session_id": null,
        "tool": {"name": args[2], "native_name": args[2], "call_id": null, "input": input,
                 "result": if args[1] == "tool.after" { json!({"example": true}) } else { json!(null) }},
        "native": {}
    }), Instant::now() + Duration::from_secs(5), &cancelled)?;
    let mut failed = false;
    // Sequential execution is this example's choice, not a Host scheduling rule.
    for step in event.steps() {
        let invocation = event.invoke(&step.step_id)?;
        let (effects, failure) = match invocation.result {
            Ok(outcome) => (outcome.as_value()["effects"].clone(), None),
            Err(error) => {
                failed = true;
                (json!(null), Some(error.to_string()))
            }
        };
        println!(
            "{}",
            json!({
                "config_revision": invocation.record.config_revision,
                "event_id": invocation.record.event_id, "request_id": invocation.record.request_id,
                "provider": invocation.record.provider, "step_id": invocation.record.step_id,
                "candidate_effects": effects, "failure": failure,
                "failure_action": invocation.failure_action.map(|action| match action { FailureAction::Report => "report", FailureAction::Block => "block" }),
                "adoption": "not_tested"
            })
        );
    }
    if failed {
        return Err("one or more Provider invocations failed".into());
    }
    Ok(())
}
