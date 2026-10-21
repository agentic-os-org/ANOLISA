//! Execute one active Adapter plan behind a stable native callback command.

use super::{
    adapter::{Adapter, HookOutput},
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
    let raw: Value = serde_json::from_slice(&input)?;
    let context = binding.native_context();
    let normalized = adapter.normalize(&context, event, &raw)?;
    let client = Client::connect(&binding.socket, transport_deadline)?;
    if client.identity() != &binding.binding.identity {
        return Err("stale stable bridge binding".into());
    }
    let id = &binding.binding.instance_id;
    let key = aw_adapter::reload::tool_key(
        normalized["session_id"].as_str().ok_or("missing session")?,
        normalized["tool"]["call_id"]
            .as_str()
            .ok_or("missing tool call")?,
    )?;
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
            client.call(
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
                },
                deadline,
            )?;
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
            return Ok(output);
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
        for group in plan.groups {
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
                                Ok(output) => output,
                                Err(_) => {
                                    let mut output = adapter
                                        .reply(event == "tool.before" && step.on_error == "block");
                                    output.stderr.extend_from_slice(
                                        b"AW step failed; query instance audit for metadata\n",
                                    );
                                    output
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
                .any(|output| matches!(output.exit, NativeExit::Code(2)));
            outputs.extend(results);
            if event == "tool.before" && blocked && binding.adapter == "qwenpaw" {
                client.call(
                    Operation::CloseEvent {
                        event_id: handle.event_id.clone(),
                        instance_id: id.clone(),
                    },
                    deadline,
                )?;
                break;
            }
        }
        Ok(aw_adapter::reload::combine(&binding.adapter, outputs)?)
    })();
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
