//! A client-to-service-to-Provider demonstration with synthetic native events.

use aw_service::{Binding, Capabilities, Client, EventHandle, Operation};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args()
        .nth(1)
        .ok_or("expected service socket path")?;
    let client = Client::connect(socket, deadline())?;
    let binding: Binding = serde_json::from_value(client.call(
        Operation::Bind {
            target: "qoder".into(),
            capabilities: Capabilities {
                adapter: "qoder".into(),
                version: "synthetic-example".into(),
                entrypoint: "local-example".into(),
                events: BTreeMap::from([
                    ("tool.before".into(), vec!["observe".into(), "block".into()]),
                    ("tool.after".into(), vec!["observe".into()]),
                ]),
            },
            cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
            environment: BTreeMap::new(),
        },
        deadline(),
    )?)?;
    let result = demonstrate(&client, &binding);
    let released = client.call(
        Operation::Unbind {
            instance_id: binding.instance_id.clone(),
        },
        deadline(),
    );
    result?;
    released?;
    Ok(())
}

fn demonstrate(client: &Client, binding: &Binding) -> Result<(), Box<dyn std::error::Error>> {
    for (name, tool) in [
        ("tool.before", "read_demo"),
        ("tool.before", "delete_demo"),
        ("tool.after", "read_demo"),
    ] {
        let expires = deadline();
        let handle: EventHandle = serde_json::from_value(client.call(Operation::OpenEvent {
            instance_id: binding.instance_id.clone(),
            event: json!({"name": name, "agent": {"adapter": "qoder", "binding_id": binding.target, "instance_id": binding.instance_id},
                "session_id": "synthetic-demo", "tool": {"name": tool, "native_name": tool, "call_id": null, "input": {},
                    "result": if name == "tool.after" { json!({"text": "demo result"}) } else { Value::Null }}, "native": {}}),
        }, expires)?)?;
        let results = handle
            .steps
            .iter()
            .map(|step| {
                client.call(
                    Operation::InvokeStep {
                        event_id: handle.event_id.clone(),
                        instance_id: binding.instance_id.clone(),
                        step_id: step.clone(),
                    },
                    expires,
                )
            })
            .collect::<Result<Vec<_>, _>>();
        let closed = client.call(
            Operation::CloseEvent {
                event_id: handle.event_id.clone(),
                instance_id: binding.instance_id.clone(),
            },
            deadline(),
        );
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"event": name, "tool": tool, "results": results?, "audit_key": handle.event_id})
            )?
        );
        closed?;
        let audit = client.call(
            Operation::Audit {
                key: handle.event_id,
            },
            deadline(),
        )?;
        if audit["terminal"] != true {
            return Err("event audit is incomplete".into());
        }
    }
    Ok(())
}
