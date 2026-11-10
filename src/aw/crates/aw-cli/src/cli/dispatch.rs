//! Execute one active Adapter plan behind a stable native callback command.

use super::{
    adapter::{Adapter, HookBinding, HookOutput},
    bridge, Arguments, Result,
};
use aw_adapter::{
    reload::{Plan, Step},
    NativeExit,
};
use aw_service::{Client, EventHandle, LifecyclePhase, Operation};
use serde_json::Value;
use std::time::{Duration, Instant};

pub(super) fn invoke(args: &Arguments, adapter: &dyn Adapter) -> Result<HookOutput> {
    args.check(
        &["--adapter", "--binding", "--event", "--step", "--on-error"],
        false,
    )?;
    let binding = bridge::binding(args.required("--binding")?)?;
    if binding.adapter != args.required("--adapter")? {
        return Err("stable callback adapter mismatch".into());
    }
    let event = args.required("--event")?;
    let transport_deadline = Instant::now() + Duration::from_secs(59);
    let input = super::hook::read_input(transport_deadline)?;
    invoke_bound(adapter, binding, event, input, transport_deadline)
}

pub(super) fn invoke_bound(
    adapter: &dyn Adapter,
    binding: HookBinding,
    event: &str,
    input: Vec<u8>,
    transport_deadline: Instant,
) -> Result<HookOutput> {
    let raw: Value = serde_json::from_slice(&input)?;
    let context = binding.native_context();
    let normalized = adapter.normalize(&context, event, &raw)?;
    let client = Client::connect(&binding.socket, transport_deadline)?;
    if client.identity() != &binding.binding.identity {
        return Err("stale stable bridge binding".into());
    }
    let id = &binding.binding.instance_id;
    let key = if event == "input.submit" {
        normalized["session_id"]
            .as_str()
            .ok_or("missing session")?
            .to_owned()
    } else {
        aw_adapter::reload::tool_key(
            normalized["session_id"].as_str().ok_or("missing session")?,
            normalized["tool"]["call_id"]
                .as_str()
                .ok_or("missing tool call")?,
        )?
    };
    // QwenPaw tracks the actual tool generator in middleware, including its
    // finally path. Qoder exposes separate native before/terminal callbacks.
    let lifecycle = |phase| -> Result<()> {
        // SAFETY: the native exec-form callback's parent is corroborated locally.
        client.call(
            Operation::BridgeLifecycle {
                instance_id: id.clone(),
                pid: unsafe { libc::getppid() } as u32,
                phase,
                key: key.clone(),
            },
            Instant::now() + Duration::from_secs(2),
        )?;
        Ok(())
    };
    if binding.adapter == "qoder" && event == "tool.before" {
        lifecycle(LifecyclePhase::ToolStart)?;
    }
    let result = (|| {
        let route = client.call(
            Operation::HookPlan {
                instance_id: id.clone(),
                event: event.into(),
            },
            transport_deadline,
        )?;
        let plan: Plan = serde_json::from_value(route["plan"].clone())?;
        let deadline =
            transport_deadline.min(Instant::now() + Duration::from_millis(plan.budget_ms + 2000));
        let (normalized, native, input) = aw_adapter::callback::normalize(
            adapter,
            &binding.adapter,
            &context,
            event,
            input,
            plan.replacement,
        )?;
        if plan.groups.is_empty() {
            // An empty policy still consumes the native correlation. A replay
            // after adding steps must not acquire a new execution budget.
            let close = if event == "input.submit" {
                Operation::CloseInputEvent {
                    instance_id: id.clone(),
                    session_id: normalized["session_id"]
                        .as_str()
                        .ok_or("missing session")?
                        .into(),
                    turn_id: normalized["turn_id"].as_str().ok_or("missing turn")?.into(),
                }
            } else {
                Operation::CloseHookEvent {
                    instance_id: id.clone(),
                    event: event.into(),
                    session_id: normalized["session_id"]
                        .as_str()
                        .ok_or("missing session")?
                        .into(),
                    call_id: normalized["tool"]["call_id"]
                        .as_str()
                        .ok_or("missing tool call")?
                        .into(),
                }
            };
            client.call(close, deadline)?;
            let mut output = adapter.reply(false);
            if plan.observe
                && client
                    .call(
                        Operation::ObserveHookEvent {
                            instance_id: id.clone(),
                            event: normalized,
                            native_input: input,
                        },
                        deadline,
                    )
                    .is_err()
            {
                output.stderr.extend_from_slice(
                    b"AW observation failed; query instance audit for metadata\n",
                );
            }
            return native_reply(&binding.adapter, event, output, plan.skill_check.as_deref());
        }
        let handle: EventHandle = serde_json::from_value(client.call(
            Operation::OpenHookEvent {
                instance_id: id.clone(),
                event: normalized,
                native_input: input,
            },
            deadline,
        )?)?;
        let mut outputs = Vec::new();
        let mut blocked = false;
        let mut groups = plan.groups.into_iter().peekable();
        while let Some(group) = groups.next() {
            let results = std::thread::scope(|scope| {
                let children = group
                    .iter()
                    .map(|step| {
                        scope.spawn(|| {
                            let result = step_output(
                                &client,
                                adapter,
                                &binding.adapter,
                                &handle,
                                event,
                                step,
                                &native,
                                deadline,
                            );
                            match result {
                                Ok(output) => (output, false),
                                Err(_) => {
                                    let block = matches!(event, "input.submit" | "tool.before")
                                        && step.on_error == "block";
                                    let mut output = adapter.reply(block);
                                    if block {
                                        // Keep native JSON vetoes visible to group scheduling.
                                        output.exit = NativeExit::Code(2);
                                    }
                                    output.stderr.extend_from_slice(
                                        b"AW step failed; query instance audit for metadata\n",
                                    );
                                    (output, true)
                                }
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                children
                    .into_iter()
                    .map(|child| child.join().map_err(|_| "callback worker failed"))
                    .collect::<std::result::Result<Vec<_>, _>>()
            })?;
            blocked |= results
                .iter()
                .any(|(output, _)| matches!(output.exit, NativeExit::Code(2)));
            let failed = results.iter().any(|(_, failed)| *failed);
            outputs.extend(results.into_iter().map(|(output, _)| output));
            if matches!(event, "input.submit" | "tool.before")
                && blocked
                && matches!(
                    binding.adapter.as_str(),
                    "qwenpaw" | "qoder-sdk" | "openclaw" | "hermes"
                )
            {
                let close = if binding.adapter == "qoder-sdk" {
                    Operation::CloseHookEvent {
                        instance_id: id.clone(),
                        event: event.into(),
                        session_id: raw["session_id"].as_str().ok_or("missing session")?.into(),
                        call_id: raw["tool_use_id"].as_str().ok_or("missing call")?.into(),
                    }
                } else if groups.peek().is_some() || failed {
                    Operation::CloseEvent {
                        event_id: handle.event_id.clone(),
                        instance_id: id.clone(),
                    }
                } else {
                    // Only successful final calls establish automatic completion.
                    break;
                };
                if let Err(error) = client.call(close, Instant::now() + Duration::from_secs(2)) {
                    if binding.adapter == "qoder-sdk"
                        || !matches!(error, aw_service::Error::Rejected(ref code) if code == "unknown_event")
                    {
                        // Cleanup uncertainty must retain the native veto and its reason.
                        if let Some(output) = outputs.last_mut() {
                            output.stderr.extend_from_slice(
                                b"AW event cleanup failed; query instance audit for metadata\n",
                            );
                        }
                    }
                }
                break;
            }
        }
        native_reply(
            &binding.adapter,
            event,
            aw_adapter::reload::combine(&binding.adapter, outputs)?,
            plan.skill_check.as_deref(),
        )
    })();
    if event == "input.submit" {
        client.call(
            Operation::CloseInputEvent {
                instance_id: id.clone(),
                session_id: normalized["session_id"]
                    .as_str()
                    .ok_or("missing session")?
                    .into(),
                turn_id: normalized["turn_id"].as_str().ok_or("missing turn")?.into(),
            },
            Instant::now() + Duration::from_secs(2),
        )?;
    }
    let denied = result.as_ref().is_ok_and(aw_adapter::reload::denied);
    if binding.adapter == "qoder"
        && (event == "tool.after" || (event == "tool.before" && (denied || result.is_err())))
    {
        lifecycle(LifecyclePhase::ToolEnd)?;
    }
    result
}

#[allow(clippy::too_many_arguments)] // Mirrors the admitted callback, without retaining service state in Adapters.
fn step_output(
    client: &Client,
    adapter: &dyn Adapter,
    adapter_id: &str,
    handle: &EventHandle,
    event: &str,
    step: &Step,
    native: &Value,
    deadline: Instant,
) -> Result<HookOutput> {
    let value = client.call(
        Operation::InvokeStep {
            event_id: handle.event_id.clone(),
            instance_id: handle.instance_id.clone(),
            step_id: step.id.clone(),
            native_environment: Some(std::env::vars_os().collect()),
        },
        deadline,
    )?;
    let output = aw_adapter::callback::response(
        adapter,
        event,
        &step.on_error,
        value,
        adapter_id,
        native,
        &mut std::io::stderr(),
    )?;
    Ok(aw_adapter::reload::adopt_step(
        adapter_id,
        event,
        &step.on_error,
        output,
    )?)
}

fn native_reply(
    adapter: &str,
    event: &str,
    mut output: HookOutput,
    skill: Option<&str>,
) -> Result<HookOutput> {
    if adapter == "hermes" && event == "tool.before" {
        let mut value: Value = if output.stdout.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(&output.stdout)?
        };
        // Only the admitted plan determines source pinning. Native/Provider replies
        // cannot disable a configured checker or invent a trusted Skill receipt.
        value
            .as_object_mut()
            .ok_or("invalid Hermes callback reply")?
            .insert("aw_skill_check".into(), serde_json::json!(skill));
        output.stdout = serde_json::to_vec(&value)?;
    }
    Ok(output)
}

#[cfg(test)]
#[path = "dispatch/closure_tests.rs"]
mod closure_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_active_plan_controls_hermes_skill_admission() {
        for (supplied, active) in [(json!(null), Some("block")), (json!("block"), None)] {
            let output = native_reply(
                "hermes",
                "tool.before",
                HookOutput {
                    exit: NativeExit::Code(0),
                    stdout: serde_json::to_vec(&json!({"aw_skill_check":supplied})).unwrap(),
                    stderr: vec![],
                },
                active,
            )
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap()["aw_skill_check"],
                json!(active)
            );
        }
    }
}
