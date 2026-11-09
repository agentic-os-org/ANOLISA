//! Native policy updates retain scheduling and vetoes independently of service state.

use super::*;
use aw_core::admission::StepExecution;

fn document(adapter: &str) -> Value {
    json!({"spec":{"agents":{"target":{"adapter":adapter}},"events":{},"execution":{"default_event_budget_ms":1000}}})
}

fn steps(event: &str, replacement: bool) -> Vec<AdmittedStep> {
    ["observe", "user", "compress"]
        .into_iter()
        .map(|id| AdmittedStep {
            event: event.into(),
            step_id: id.into(),
            provider: id.into(),
            event_tools: vec!["*".into()],
            step_tools: vec!["*".into()],
            on_error: "report".into(),
            execution: StepExecution::Provider {
                operation: "record".into(),
                effects: vec![if replacement && id == "compress" {
                    "replace_result"
                } else {
                    "observe"
                }
                .into()],
            },
        })
        .collect()
}

#[test]
fn native_plans_preserve_qoder_parallelism_and_qwenpaw_onion_order() {
    let selected = steps("tool.after", false);
    let mut qoder = document("qoder");
    assert_eq!(
        plan(&qoder, "target", &selected, "tool.after")
            .unwrap()
            .groups[0]
            .len(),
        3
    );
    qoder["spec"]["agents"]["target"]["qoder"] = json!({"sequential":true});
    assert_eq!(
        plan(&qoder, "target", &selected, "tool.after")
            .unwrap()
            .groups
            .len(),
        3
    );
    let qwenpaw = document("qwenpaw");
    let after = plan(&qwenpaw, "target", &selected, "tool.after").unwrap();
    assert_eq!(
        after
            .groups
            .iter()
            .map(|g| g[0].id.as_str())
            .collect::<Vec<_>>(),
        ["compress", "user", "observe"]
    );
    let after = plan(&qwenpaw, "target", &steps("tool.after", true), "tool.after").unwrap();
    assert!(after.replacement);
    assert_eq!(
        after
            .groups
            .iter()
            .map(|g| g[0].id.as_str())
            .collect::<Vec<_>>(),
        ["observe", "user", "compress"]
    );
    assert!(plan(
        &document("qoder"),
        "target",
        &steps("tool.after", true),
        "tool.after"
    )
    .is_err());
}

#[test]
fn empty_initial_policy_still_registers_both_stable_routes() {
    let routes = routes();
    assert_eq!(routes.len(), 2);
    for route in routes.values() {
        assert!(route.steps.contains_key(DISPATCH));
    }
    for adapter in ["qoder", "qwenpaw", "qoder-sdk"] {
        for event in ["tool.before", "tool.after"] {
            let empty = plan(&document(adapter), "target", &[], event).unwrap();
            assert!(empty.groups.is_empty());
            assert_eq!(empty.budget_ms, 1000);
            assert_eq!(empty.observe, adapter != "qoder", "{adapter}: {event}");
        }
    }
}

#[test]
fn native_routes_use_only_the_targets_admitted_steps() {
    let validator = aw_config::Validator::new().unwrap();
    let mut value = validator
        .parse(include_bytes!(
            "../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["agents"] = json!({
        "sdk":{"adapter":"qoder-sdk","argv":["/bin/true"]},
        "app":{"adapter":"qwenpaw","argv":["/bin/true"]}
    });
    value["spec"]["providers"]["hook"] = json!({
        "transport":{"type":"stdio","location":"agent","argv":["/bin/true"]},
        "timeout_ms":1000,"max_output_bytes":4096
    });
    value["spec"]["events"]["tool.after"]["steps"] = json!([
        {"id":"app","provider":"hook","effects":["observe"],
         "on_error":"withhold_result","match":{"tools":["native:qwenpaw:tool"]}},
        {"id":"sdk","provider":"hook","effects":["observe"],
         "on_error":"report","match":{"tools":["native:qoder-sdk:Bash"]}}
    ]);
    let config = validator
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let capabilities = AdapterCapabilities {
        adapter: "qoder-sdk".into(),
        version: "1.0.56".into(),
        entrypoint: "sdk-worker".into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
        ]),
        failure_actions: BTreeMap::new(),
    };
    let selected = aw_core::admission::preflight(&config, "sdk", &capabilities).unwrap();
    let native = adapter::native_steps(&selected, "tool.after", false);
    assert_eq!(native.len(), 1);
    assert_eq!(native[0].step_id, "sdk");
    let dynamic = plan(config.as_value(), "sdk", &selected, "tool.after").unwrap();
    assert_eq!(dynamic.groups.len(), 1);
    assert_eq!(dynamic.groups[0][0].id, "sdk");
    assert!(!dynamic.replacement);
}

#[test]
fn qoder_stop_and_permission_wait_do_not_claim_request_idle() {
    let context = NativeContext {
        target: "target".into(),
        instance_id: "instance".into(),
        cwd: "/private/cwd".into(),
    };
    let mut payload = json!({"cwd":"/private/cwd","session_id":"native","hook_event_name":"Stop"});
    assert!(qoder_lifecycle(&context, &payload).is_err());
    payload["hook_event_name"] = json!("Notification");
    payload["notification_type"] = json!("permission_prompt");
    assert!(qoder_lifecycle(&context, &payload).is_err());
    payload["notification_type"] = json!("idle_prompt");
    assert_eq!(
        qoder_lifecycle(&context, &payload).unwrap(),
        ("request_end", "native".into())
    );
    payload["cwd"] = json!("/changed");
    assert!(qoder_lifecycle(&context, &payload).is_err());
}

#[test]
fn native_denial_survives_other_replies_and_never_becomes_approval() {
    let output = combine(
        "qoder",
        vec![
            HookOutput {
                exit: NativeExit::Code(2),
                stdout: vec![],
                stderr: b"denied\n".to_vec(),
            },
            HookOutput {
                exit: NativeExit::Code(0),
                stdout: br#"{"decision":"allow"}"#.to_vec(),
                stderr: vec![],
            },
        ],
    )
    .unwrap();
    assert!(matches!(output.exit, NativeExit::Code(2)));
    assert!(!String::from_utf8(output.stdout).unwrap().contains("allow"));
    assert_eq!(output.stderr, b"denied\n");
    let output = combine(
        "qoder",
        vec![
            HookOutput {
                exit: NativeExit::Code(0),
                stdout: br#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}"#.to_vec(),
                stderr: vec![],
            },
            HookOutput {
                exit: NativeExit::Code(0),
                stdout: br#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#.to_vec(),
                stderr: vec![],
            },
        ],
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["hookSpecificOutput"]
            ["permissionDecision"],
        "deny"
    );
}

#[test]
fn per_step_failure_actions_preserve_other_vetoes_and_final_replacements() {
    let failure = || HookOutput {
        exit: NativeExit::Code(3),
        stdout: b"ignored failure output".to_vec(),
        stderr: b"diagnostic".to_vec(),
    };
    for adapter in ["qwenpaw", "qoder-sdk"] {
        let report = adopt_step(adapter, "tool.before", "report", failure()).unwrap();
        assert!(matches!(report.exit, NativeExit::Code(0)));
        assert!(report.stdout.is_empty());
        assert!(matches!(
            adopt_step(adapter, "tool.before", "block", failure())
                .unwrap()
                .exit,
            NativeExit::Code(2)
        ));
    }
    let veto = HookOutput { exit: NativeExit::Code(0), stdout: br#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"native veto"}}"#.to_vec(), stderr: vec![] };
    let veto = adopt_step("qoder", "tool.before", "report", veto).unwrap();
    let report = adopt_step("qoder", "tool.before", "report", failure()).unwrap();
    let combined = combine("qoder", vec![veto, report]).unwrap();
    assert!(denied(&combined));
    let replacement = HookOutput {
        exit: NativeExit::Code(0),
        stdout: br#"{"aw_replacement":{"content":[{"type":"text","text":"compact"}]}}"#.to_vec(),
        stderr: vec![],
    };
    let combined = combine(
        "qwenpaw",
        vec![
            adopt_step("qwenpaw", "tool.after", "report", failure()).unwrap(),
            replacement,
        ],
    )
    .unwrap();
    assert!(matches!(combined.exit, NativeExit::Code(0)));
    assert_eq!(
        serde_json::from_slice::<Value>(&combined.stdout).unwrap()["aw_replacement"]["content"][0]
            ["text"],
        "compact"
    );
}

#[test]
fn qoder_permission_priority_and_nested_override_survive_dispatch() {
    for (first, second, expected) in [
        (
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}),
            "ask",
        ),
        (
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            "ask",
        ),
        (
            json!({"decision":"deny", "hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            json!({"decision":"allow"}),
            "ask",
        ),
        (
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            json!({"decision":"allow"}),
            "ask",
        ),
        (
            json!({"decision":"deny"}),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            "deny",
        ),
        (
            json!({"decision":"deny", "hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}),
            json!({"decision":"approve"}),
            "allow",
        ),
        (
            json!({"decision":"block"}),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            "deny",
        ),
    ] {
        let outputs = [first, second]
            .into_iter()
            .map(|value| {
                adopt_step(
                    "qoder",
                    "tool.before",
                    "report",
                    HookOutput {
                        exit: NativeExit::Code(0),
                        stdout: serde_json::to_vec(&value).unwrap(),
                        stderr: vec![],
                    },
                )
                .unwrap()
            })
            .collect();
        let output = combine("qoder", outputs).unwrap();
        if expected == "deny" {
            assert!(denied(&output));
        } else {
            assert!(!denied(&output));
            assert!(matches!(output.exit, NativeExit::Code(0)));
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(value.get("decision").is_none());
            assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
            let permission = value["hookSpecificOutput"]["permissionDecision"]
                .as_str()
                .or(value["decision"].as_str());
            assert_eq!(permission, Some(expected));
        }
    }
}

#[test]
fn qoder_preserves_ordered_additional_context_and_final_replacement() {
    let outputs = ["first-context", "second-context"]
        .into_iter()
        .map(|context| HookOutput {
            exit: NativeExit::Code(0),
            stdout: serde_json::to_vec(&json!({"hookSpecificOutput":{
                "hookEventName":"PostToolUse", "additionalContext":context,
                "updatedToolOutput":context,
            }}))
            .unwrap(),
            stderr: vec![],
        })
        .collect();
    let output = combine("qoder", outputs).unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["hookSpecificOutput"]["additionalContext"],
        "first-context\nsecond-context"
    );
    assert_eq!(
        value["hookSpecificOutput"]["updatedToolOutput"],
        "second-context"
    );
}

#[test]
fn qoder_sdk_reload_preserves_the_first_denial_reason() {
    let mut veto = HookOutput {
        exit: NativeExit::Code(2),
        stdout: vec![],
        stderr: vec![],
    };
    let reason = "Retry with arguments: {\"command\":\"rtk candidate\"}";
    adapter::block_reason("qoder-sdk", &mut veto, reason).unwrap();
    let veto = adopt_step("qoder-sdk", "tool.before", "block", veto).unwrap();
    let output = combine(
        "qoder-sdk",
        vec![
            veto,
            HookOutput {
                exit: NativeExit::Code(0),
                stdout: br#"{"decision":"allow"}"#.to_vec(),
                stderr: vec![],
            },
            HookOutput {
                exit: NativeExit::Code(2),
                stdout: br#""another denial""#.to_vec(),
                stderr: vec![],
            },
        ],
    )
    .unwrap();
    assert!(matches!(output.exit, NativeExit::Code(2)));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        reason
    );
}

#[test]
fn policy_updates_use_native_entrypoints_across_framework_releases() {
    for (adapter, entrypoint) in [
        ("qoder", "tui"),
        ("qwenpaw", "app"),
        ("qoder-sdk", "sdk-worker"),
    ] {
        for version in ["1.0.55", "1.0.57", "3.0.0-preview", "2026.10.29"] {
            let mut capabilities = AdapterCapabilities {
                adapter: adapter.into(),
                version: version.into(),
                entrypoint: entrypoint.into(),
                events: BTreeMap::new(),
                failure_actions: BTreeMap::new(),
            };
            assert!(supported(&capabilities).is_ok());
            assert_eq!(
                resident_supported(&capabilities).is_ok(),
                adapter != "qoder"
            );
            capabilities.entrypoint = "unsupported".into();
            assert!(supported(&capabilities).is_err());
        }
    }
}

#[test]
fn hermes_native_json_veto_survives_later_approval() {
    for field in ["action", "decision"] {
        for status in [0, 3] {
            let veto = adopt_step(
                "hermes",
                "tool.before",
                "report",
                HookOutput {
                    exit: NativeExit::Code(status),
                    stdout: serde_json::to_vec(&json!({field:"block","message":"native veto"}))
                        .unwrap(),
                    stderr: vec![],
                },
            )
            .unwrap();
            assert!(matches!(veto.exit, NativeExit::Code(2)));
            let output = combine(
                "hermes",
                vec![
                    veto,
                    HookOutput {
                        exit: NativeExit::Code(0),
                        stdout: br#"{"action":"approve"}"#.to_vec(),
                        stderr: vec![],
                    },
                ],
            )
            .unwrap();
            assert!(matches!(output.exit, NativeExit::Code(2)));
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value[field], "block");
            assert_eq!(value["message"], "native veto");
        }
    }
}

#[test]
fn hermes_success_keeps_the_submission_json_envelope() {
    let output = combine(
        "hermes",
        vec![HookOutput {
            exit: NativeExit::Code(0),
            stdout: b"{}".to_vec(),
            stderr: vec![],
        }],
    )
    .unwrap();
    assert!(matches!(output.exit, NativeExit::Code(0)));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({})
    );
}

#[test]
fn hermes_skill_plan_selects_only_matching_checks_and_preserves_budget() {
    let mut value = document("hermes");
    value["spec"]["execution"]["default_event_budget_ms"] = json!(55000);
    let mut selected = steps("tool.before", false);
    for step in &mut selected {
        step.execution = StepExecution::Provider {
            operation: "check_skill".into(),
            effects: vec!["observe".into(), "block".into()],
        };
        step.step_tools = vec!["native:hermes:skill_view".into()];
    }
    assert_eq!(
        plan(&value, "target", &[], "tool.before")
            .unwrap()
            .skill_check,
        None
    );
    let report = plan(&value, "target", &selected, "tool.before").unwrap();
    assert_eq!(report.skill_check.as_deref(), Some("report"));
    assert_eq!(report.budget_ms, 55000);
    selected[0].on_error = "block".into();
    assert_eq!(
        plan(&value, "target", &selected, "tool.before")
            .unwrap()
            .skill_check
            .as_deref(),
        Some("block")
    );
    selected[0].step_tools = vec!["native:hermes:terminal".into()];
    assert_eq!(
        plan(&value, "target", &selected, "tool.before")
            .unwrap()
            .skill_check
            .as_deref(),
        Some("report")
    );
    value["spec"]["execution"]["default_event_budget_ms"] = json!(55001);
    assert!(plan(&value, "target", &selected, "tool.before").is_err());
}

#[test]
fn openclaw_dispatch_preserves_first_denial_and_tokenless_fresh_arguments() {
    let arguments = json!({"command":"rtk git status --short","cwd":"/work tree"});
    let retry = format!(
        "[tokenless:rewrite] Retry this tool with arguments: {}",
        serde_json::to_string(&arguments).unwrap()
    );
    for reason in ["Custom security refusal".to_owned(), retry.clone()] {
        let mut veto = HookOutput {
            exit: NativeExit::Code(0),
            stdout: br#"{"block":true,"nativeMetadata":"retained"}"#.to_vec(),
            stderr: vec![],
        };
        adapter::block_reason("openclaw", &mut veto, &reason).unwrap();
        let output = combine(
            "openclaw",
            vec![
                adopt_step("openclaw", "tool.before", "block", veto).unwrap(),
                HookOutput {
                    exit: NativeExit::Code(0),
                    stdout: br#"{"block":false,"blockReason":"later approval"}"#.to_vec(),
                    stderr: vec![],
                },
                HookOutput {
                    exit: NativeExit::Code(2),
                    stdout: br#"{"block":true,"blockReason":"later denial"}"#.to_vec(),
                    stderr: vec![],
                },
            ],
        )
        .unwrap();
        assert!(matches!(output.exit, NativeExit::Code(0)));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["block"], true);
        assert_eq!(value["blockReason"], reason);
        assert_eq!(value["nativeMetadata"], "retained");
        assert!(value.get("params").is_none());
        if reason == retry {
            let corrected = reason
                .strip_prefix("[tokenless:rewrite] Retry this tool with arguments: ")
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(corrected).unwrap(), arguments);
        }
    }
}

#[test]
fn openclaw_dispatch_failure_without_a_native_reason_keeps_a_block_envelope() {
    for stdout in [
        vec![],
        b"invalid".to_vec(),
        br#"{"blockReason":" "}"#.to_vec(),
    ] {
        let output = combine(
            "openclaw",
            vec![HookOutput {
                exit: NativeExit::Code(2),
                stdout,
                stderr: vec![],
            }],
        )
        .unwrap();
        assert!(matches!(output.exit, NativeExit::Code(0)));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["block"], true);
        assert_eq!(value["blockReason"], "Tool blocked by AW policy");
    }
}

#[test]
fn native_execution_claims_match_each_bridges_lifecycle_identity() {
    let native = json!({"native":{"context":{"sessionId":"session","runId":"run"},
        "event":{"runId":"run"}}});
    assert_eq!(
        execution_claim("openclaw", &native).unwrap(),
        ExecutionClaim::Request(tool_key("session", "run").unwrap())
    );
    let mut fallback = native.clone();
    fallback["native"]["context"]
        .as_object_mut()
        .unwrap()
        .remove("runId");
    assert_eq!(
        execution_claim("openclaw", &fallback).unwrap(),
        ExecutionClaim::Request(tool_key("session", "run").unwrap())
    );
    let mut mismatch = native.clone();
    mismatch["native"]["event"]["runId"] = json!("another-run");
    assert!(execution_claim("openclaw", &mismatch).is_err());
    for identity in [Value::Null, json!(""), json!("x".repeat(513))] {
        let mut invalid = native.clone();
        invalid["native"]["context"]["sessionId"] = identity;
        assert!(execution_claim("openclaw", &invalid).is_err());
    }
    assert_eq!(
        execution_claim(
            "qoder",
            &json!({"name":"input.submit","session_id":"session"})
        )
        .unwrap(),
        ExecutionClaim::Request("session".into())
    );
    for event in ["tool.before", "tool.after"] {
        assert_eq!(
            execution_claim(
                "qoder",
                &json!({"name":event,"session_id":"session",
            "tool":{"call_id":"call"}})
            )
            .unwrap(),
            ExecutionClaim::Tool(tool_key("session", "call").unwrap())
        );
    }
    assert!(execution_claim(
        "qoder",
        &json!({"name":"tool.before","session_id":"session"})
    )
    .is_err());
    assert_eq!(
        execution_claim("hermes", &json!({})).unwrap(),
        ExecutionClaim::SerialRequest
    );
    assert!(execution_claim("unsupported", &native).is_err());
}
