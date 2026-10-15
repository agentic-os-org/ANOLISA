//! Bound retry reasons retain native denials and fail-closed security callbacks.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use aw_service::{Binding, Capabilities, Operation};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, time::Instant};
use support::{Fixture, Service};

fn blocked(adapter: &str, output: &std::process::Output) -> bool {
    match adapter {
        "qoder" | "qwenpaw" | "hermes" => output.status.code() == Some(2),
        _ => serde_json::from_slice::<Value>(&output.stdout).unwrap()["block"] == true,
    }
}

#[test]
fn retry_neutrality_cannot_undo_security_denials_or_fail_closed_errors() {
    for adapter in ["qoder", "openclaw", "qwenpaw", "hermes"] {
        let f = Fixture::new();
        let provider = f.root.join("policy.py");
        fs::write(&provider, r#"import json, sys
r = json.load(sys.stdin)
reply = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'ok'}
if r['method'] == 'describe':
    reply['operations'] = [{'name':name,'events':['tool.before'],'effects':['block']} for name in ['scan','pre_tool']]
elif r['method'] == 'invoke':
    command = r['event']['tool']['input']['command']
    reply['input_digest'] = r['input_digest']
    reply['effects'] = []
    if r['operation'] == 'pre_tool' and command == 'original':
        reply['effects'] = [{'type':'block','reason_code':'tokenless_retry','text':'Retry with arguments: {"command":"rtk candidate"}'}]
    if r['operation'] == 'scan' and command == 'rtk candidate':
        reply['effects'] = [{'type':'block','reason_code':'security_denied'}]
    if r['operation'] == 'scan' and command == 'scan-failure':
        reply = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'error','error_code':'scanner_unavailable'}
print(json.dumps(reply))
"#).unwrap();
        let mut document = f.document();
        document["spec"]["agents"] = json!({"agent":{"adapter":adapter,"argv":["/bin/true"]}});
        document["spec"]["providers"] = json!({"policy":{"protocol":"aw-provider/v1alpha1","transport":{"type":"stdio","location":"agent","argv":[f.python,provider]},"timeout_ms":2000,"max_output_bytes":4096,"config":{}}});
        document["spec"]["events"] = json!({"tool.before":{"enabled":true,"steps":[
            {"id":"security","provider":"policy","operation":"scan","effects":["block"],"on_error":"block"},
            {"id":"rewrite","provider":"policy","operation":"pre_tool","effects":["block"],"on_error":"report"}]}});
        let service = Service::start(&f, &document);
        let deadline = || Instant::now() + support::TIMEOUT;
        let binding: Binding = serde_json::from_value(
            service
                .client
                .call(
                    Operation::Bind {
                        target: "agent".into(),
                        capabilities: Capabilities {
                            adapter: adapter.into(),
                            version: "fixture".into(),
                            entrypoint: "fixture".into(),
                            events: BTreeMap::from([("tool.before".into(), vec!["block".into()])]),
                        },
                        cwd: f.root.to_str().unwrap().into(),
                        environment: BTreeMap::new(),
                    },
                    deadline(),
                )
                .unwrap(),
        )
        .unwrap();
        let path = f.root.join("binding.json");
        fs::write(&path, json!({"adapter":adapter,"binding":binding,"socket":f.root.join("state/aw.sock"),"cwd":f.root,"events":{"tool.before":{"budget_ms":5000,"steps":{"security":"block","rewrite":"report"}}}}).to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        for (attempt, command) in [
            ("first", "original"),
            ("retry", "rtk candidate"),
            ("error", "scan-failure"),
        ] {
            let input = json!({"command":command,"cwd":"preserved"});
            let native = match adapter {
                "qoder" => {
                    json!({"hook_event_name":"PreToolUse","cwd":f.root,"session_id":"session","tool_use_id":attempt,"tool_name":"Bash","tool_input":input})
                }
                "openclaw" => {
                    json!({"hook":"before_tool_call","event":{"toolName":"exec","params":input,"toolCallId":attempt,"runId":"run"},"context":{"sessionId":"session","runId":"run","toolCallId":attempt}})
                }
                "qwenpaw" => {
                    json!({"event":"tool.before","session_id":"session","tool_call":{"name":"exec","id":attempt,"input":input.to_string()}})
                }
                _ => {
                    json!({"hook_event_name":"pre_tool_call","cwd":f.root,"session_id":"session","tool_name":"terminal","tool_input":input,"extra":{"tool_call_id":attempt,"api_request_id":"api"}})
                }
            };
            let mut outputs = Vec::new();
            for (step, on_error) in [("security", "block"), ("rewrite", "report")] {
                let mut callback = f.command();
                callback
                    .args(["hook", "--adapter", adapter, "--binding"])
                    .arg(&path)
                    .args([
                        "--event",
                        "tool.before",
                        "--step",
                        step,
                        "--on-error",
                        on_error,
                    ]);
                outputs.push(f.run_input(callback, Some(&serde_json::to_vec(&native).unwrap())));
            }
            assert!(
                outputs.iter().any(|out| blocked(adapter, out)),
                "{adapter}/{attempt}"
            );
            assert_eq!(blocked(adapter, &outputs[1]), attempt == "first");
            if attempt == "first" {
                let all = format!(
                    "{}{}",
                    String::from_utf8_lossy(&outputs[1].stdout),
                    String::from_utf8_lossy(&outputs[1].stderr)
                );
                assert!(all.contains("Retry with arguments"), "{adapter}: {all}");
                assert!(!all.contains("updatedInput"));
                assert!(!all.contains("approve"));
            }
        }
        for entry in fs::read_dir(f.root.join("state/journal")).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains("Retry with arguments"));
            assert!(!text.contains("rtk candidate"));
        }
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: binding.instance_id,
                },
                deadline(),
            )
            .unwrap();
        service.released(&f);
    }
}
