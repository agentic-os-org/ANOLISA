use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use cosh_shell::adapter::{AgentRunPoll, CoshCoreAdapter};
use cosh_shell::types::{
    AgentMode, AgentRequest, CommandBlock, CommandStatus, CoshApprovalMode, OutputRefs,
};
use serde_json::{json, Value};

fn request(workspace: &Path) -> AgentRequest {
    let workspace = workspace.to_string_lossy().into_owned();
    AgentRequest {
        id: "registry-receipt".into(),
        session_id: "session".into(),
        command_block: CommandBlock {
            id: "command".into(),
            session_id: "session".into(),
            command: "test".into(),
            origin: Default::default(),
            cwd: workspace.clone(),
            end_cwd: workspace,
            started_at_ms: 0,
            ended_at_ms: 1,
            duration_ms: 1,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("test registry receipts".into()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

fn adapter_fixture(response: &Value) -> (tempfile::TempDir, CoshCoreAdapter) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("core.py");
    fs::write(root.path().join("response.json"), response.to_string()).unwrap();
    fs::write(&script, r#"#!/usr/bin/env python3
import json, pathlib, sys
root = pathlib.Path(__file__).parent
short = '--registry' in sys.argv
session = '00000000-0000-4000-8000-000000000000'
for line in sys.stdin:
    message = json.loads(line)
    if message.get('type') == 'registry_request':
        response = json.loads((root / 'response.json').read_text())
        response.update(type='registry_response', request_id=message['request_id'])
        with (root / 'requests.jsonl').open('a') as output:
            output.write(json.dumps(dict(mode='short' if short else 'live', request=message)) + '\n')
        print(json.dumps(response), flush=True)
        if short:
            break
    elif message.get('type') == 'control_request':
        if message.get('request', {}).get('subtype') == 'shutdown':
            break
        print(json.dumps(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=dict(subtype='initialize', capabilities={})))), flush=True)
    elif message.get('type') == 'user':
        print(json.dumps(dict(type='system', subtype='init', session_id=session, model='mock', tools=[])), flush=True)
        print(json.dumps(dict(type='result', subtype='success', session_id=session, is_error=False, result='done')), flush=True)
"#).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let adapter = CoshCoreAdapter::new(script.to_string_lossy().into_owned(), true);
    (root, adapter)
}

fn check_receipt(response: Value, live: bool) {
    let (root, adapter) = adapter_fixture(&response);
    if live {
        let handle = adapter.start_cancellable(request(root.path()), CoshApprovalMode::Auto);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match handle.poll_event_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(AgentRunPoll::Finished) => break,
                Ok(AgentRunPoll::Event(_)) => {}
                Ok(AgentRunPoll::Timeout) => panic!("persistent core did not become idle"),
                Err(error) => panic!("persistent core failed: {}", error.message),
            }
        }
    }
    for action in ["activate", "activate_trial", "set_trial_preference"] {
        let result = adapter.registry_query("auth", action, Value::Null);
        match response.get("success").and_then(Value::as_bool) {
            Some(false) => assert_eq!(result.unwrap_err(), "provider not found"),
            Some(true) => assert_eq!(result.unwrap(), json!({"saved":true})),
            None => {
                let error = result.unwrap_err();
                assert!(
                    error.contains("no boolean success"),
                    "{action}: {response}: {error}"
                );
            }
        }
        if live && response.get("success").and_then(Value::as_bool).is_none() {
            break;
        }
    }
    let calls = fs::read_to_string(root.path().join("requests.jsonl")).unwrap();
    for line in calls.lines() {
        let call: Value = serde_json::from_str(line).unwrap();
        assert_eq!(call["mode"], if live { "live" } else { "short" });
    }
    assert!(!calls.is_empty());
}

#[test]
fn short_registry_receipts_require_boolean_success() {
    for response in [
        json!({}),
        json!({"success":null}),
        json!({"success":"false"}),
        json!({"success":0}),
    ] {
        check_receipt(response, false);
    }
}

#[test]
fn live_registry_receipts_require_boolean_success() {
    for response in [
        json!({}),
        json!({"success":null}),
        json!({"success":"false"}),
        json!({"success":0}),
    ] {
        check_receipt(response, true);
    }
}

#[test]
fn valid_false_receipts_preserve_the_owner_rejection() {
    for live in [false, true] {
        check_receipt(json!({"success":false,"error":"provider not found"}), live);
    }
}

#[test]
fn valid_true_receipts_preserve_the_owner_data() {
    for live in [false, true] {
        check_receipt(json!({"success":true,"data":{"saved":true}}), live);
    }
}
