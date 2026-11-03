//! Public CLI fixtures exercise binding, capabilities, lossless policy and failure boundaries.

use aw_provider::{Protocol, Reply, VERSION};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

struct Fixture {
    _root: tempfile::TempDir,
    cli: PathBuf,
    state: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let build = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tokenless-validation");
        fs::create_dir_all(&build).unwrap();
        let root = tempfile::tempdir_in(build).unwrap();
        let cli = root.path().join("tokenless");
        fs::write(
            &cli,
            format!(
                r#"#!/usr/bin/python3
import json, os, pathlib, sys, time
assert sys.argv[1:] == ['compress']
request = json.load(sys.stdin)
pathlib.Path(__file__).with_suffix('.request').write_text(json.dumps(request))
assert request['protocol_version'] == 2 and request['operation'] == 'post_tool'
assert request['input']['capabilities']['recovery'] == {{'kind':'none'}}
assert request['input']['output_optimization'] == 'none'
assert os.environ['TOKENLESS_DATA_DIR'].endswith('state')
mode = {mode:?}
if mode == 'failed': sys.exit(1)
if mode == 'slow': time.sleep(2)
if mode == 'overflow': print('x' * (1024 * 1024 + 1)); sys.exit(0)
disposition = 'applied' if mode not in ('no_savings', 'dry_run') else mode
output = request['input']['content'] if disposition != 'applied' else 'compact'
if mode == 'empty': output = ''
if mode == 'inflated': output = request['input']['content'] * 2
if mode == 'same_size': output = 'x' * len(request['input']['content'])
attribution = request['attribution'] if mode != 'wrong_identity' else {{'agent_id':'other'}}
if mode == 'cross_binding': attribution['agent_id'] = 'target-b'
if mode == 'framework_identity': attribution['agent_id'] = 'qoder'
if mode == 'wrong_session': attribution['session_id'] = 'other-session'
if mode == 'wrong_call': attribution['tool_use_id'] = 'other-call'
print(json.dumps({{'protocol_version':2,'operation':'post_tool','attribution':attribution,
    'result':{{'disposition':disposition,'output':output}}}}))
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("state");
        Self {
            _root: root,
            cli,
            state,
        }
    }
    fn run(&self, value: Value, timeout: Duration) -> Value {
        let mut out = Vec::new();
        aw_provider_tokenless::run(
            &mut serde_json::to_vec(&value).unwrap().as_slice(),
            &mut out,
            &self.cli,
            &self.state,
            timeout,
        )
        .unwrap();
        serde_json::from_slice(&out).unwrap()
    }
}

fn invoke(projected: bool) -> Value {
    let mut value = json!({"api_version":VERSION,"method":"invoke","request_id":"call","operation":"post_tool","config":{"version":1},"config_revision":"a".repeat(64),"budget_ms":1000,"allowed_effects":["replace_result"],"event":{"name":"tool.after","agent":{"adapter":"qoder","binding_id":"target","instance_id":"instance"},"session_id":"session","tool":{"name":"Bash","native_name":"Bash","call_id":"tool-call","input":{"command":"echo text"},"result":"original"},"native":{}}});
    if projected {
        value["event"]["tool"]["result_projection"] = json!({"text":"original","status":"success","content_origin":"command_output","command":"echo text"});
    }
    Protocol::new()
        .unwrap()
        .bind_invocation(value)
        .unwrap()
        .as_value()
        .clone()
}

#[test]
fn applied_cli_result_is_a_bound_text_replacement() {
    let fixture = Fixture::new("applied");
    let request = invoke(true);
    let reply = fixture.run(request.clone(), Duration::from_secs(1));
    let Reply::Invocation(outcome) = Protocol::new()
        .unwrap()
        .check_response(
            &Protocol::new()
                .unwrap()
                .parse_request(&serde_json::to_vec(&request).unwrap())
                .unwrap(),
            &serde_json::to_vec(&reply).unwrap(),
        )
        .unwrap()
    else {
        panic!("wrong method");
    };
    assert_eq!(outcome.as_value()["effects"][0]["text"], "compact");
    assert_eq!(outcome.as_value()["effects"][0]["type"], "replace_result");
}

#[test]
fn cli_attribution_distinguishes_bindings_and_preserves_native_ids() {
    for binding in ["target-a", "target-b"] {
        let fixture = Fixture::new("applied");
        let mut value = invoke(true);
        value.as_object_mut().unwrap().remove("input_digest");
        value["event"]["agent"]["binding_id"] = json!(binding);
        let request = Protocol::new().unwrap().bind_invocation(value).unwrap();
        let reply = fixture.run(request.as_value().clone(), Duration::from_secs(1));
        assert_eq!(reply["effects"][0]["text"], "compact", "{reply}");
        let wire: Value =
            serde_json::from_slice(&fs::read(fixture.cli.with_extension("request")).unwrap())
                .unwrap();
        assert_eq!(
            wire["attribution"],
            json!({
                "agent_id": binding, "session_id": "session", "tool_use_id": "tool-call"
            })
        );
    }
    for mode in [
        "cross_binding",
        "framework_identity",
        "wrong_session",
        "wrong_call",
    ] {
        let fixture = Fixture::new(mode);
        let mut value = invoke(true);
        value.as_object_mut().unwrap().remove("input_digest");
        value["event"]["agent"]["binding_id"] = json!("target-a");
        let request = Protocol::new().unwrap().bind_invocation(value).unwrap();
        let reply = fixture.run(request.as_value().clone(), Duration::from_secs(1));
        assert_eq!(
            reply["error_code"], "invalid_compression_result",
            "{mode}: {reply}"
        );
        assert!(reply.get("effects").is_none());
    }
}

#[test]
fn unsupported_slots_and_failed_tools_never_call_cli() {
    let fixture = Fixture::new("failed");
    assert_eq!(
        fixture.run(invoke(false), Duration::from_secs(1))["effects"],
        json!([])
    );
    for status in ["error", "denied", "interrupted"] {
        let mut request = invoke(true);
        request.as_object_mut().unwrap().remove("input_digest");
        request["event"]["tool"]["result_projection"]["status"] = json!(status);
        let request = Protocol::new()
            .unwrap()
            .bind_invocation(request)
            .unwrap()
            .as_value()
            .clone();
        assert_eq!(
            fixture.run(request, Duration::from_secs(1))["effects"],
            json!([])
        );
    }
}

#[test]
fn cli_failures_and_wrong_identity_remain_visible_errors() {
    for (mode, expected) in [
        ("failed", "cli_failed"),
        ("wrong_identity", "invalid_compression_result"),
        ("overflow", "cli_output_limit"),
        ("slow", "deadline_exceeded"),
        ("empty", "non_shrinking_replacement"),
        ("inflated", "non_shrinking_replacement"),
        ("same_size", "non_shrinking_replacement"),
    ] {
        let fixture = Fixture::new(mode);
        let reply = fixture.run(invoke(true), Duration::from_millis(300));
        assert_eq!(reply["status"], "error", "{mode}");
        assert_eq!(reply["error_code"], expected, "{mode}");
        assert!(reply.get("effects").is_none());
    }
}

#[test]
fn dry_run_and_no_savings_keep_original_result() {
    for mode in ["dry_run", "no_savings"] {
        assert_eq!(
            Fixture::new(mode).run(invoke(true), Duration::from_secs(1))["effects"],
            json!([])
        );
    }
}

#[test]
fn describe_and_config_validation_are_offline_and_strict() {
    let fixture = Fixture::new("failed");
    assert_eq!(
        fixture.run(
            json!({"api_version":VERSION,"method":"describe","request_id":"describe"}),
            Duration::ZERO
        )["operations"][0]["effects"],
        json!(["replace_result"])
    );
    for config in [
        json!({"version":2}),
        json!({"version":1,"recovery":"shell"}),
    ] {
        assert_eq!(fixture.run(json!({"api_version":VERSION,"method":"validate_config","request_id":"config","config":config}),Duration::ZERO)["error_code"],"invalid_config");
    }
}

#[test]
#[ignore = "requires a built Tokenless Protocol v2 CLI; package CI runs this test"]
fn bundled_cli_compresses_losslessly_without_a_recovery_entry() {
    let cli = PathBuf::from(
        std::env::var_os("AW_TOKENLESS_TEST_CLI")
            .expect("set AW_TOKENLESS_TEST_CLI to the built CLI"),
    );
    let build = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tokenless-validation");
    fs::create_dir_all(&build).unwrap();
    let root = tempfile::tempdir_in(build).unwrap();
    let state = root.path().join("state");
    let fixture = Fixture {
        _root: root,
        cli,
        state,
    };
    let original = json!({"unicode":"你好", "numbers":(0..30).collect::<Vec<_>>(), "ok":true, "nested":{"value":"kept"}});
    let content = serde_json::to_string_pretty(&original).unwrap();
    let mut request = invoke(true);
    request.as_object_mut().unwrap().remove("input_digest");
    request["event"]["tool"]["result_projection"] =
        json!({"text":content,"status":"success","content_origin":"api_response"});
    let request = Protocol::new()
        .unwrap()
        .bind_invocation(request)
        .unwrap()
        .as_value()
        .clone();
    let reply = fixture.run(request, Duration::from_secs(1));
    assert_eq!(reply["status"], "ok", "{reply}");
    assert_eq!(reply["effects"].as_array().unwrap().len(), 1, "{reply}");
    let compressed = reply["effects"][0]["text"].as_str().unwrap();
    assert!(compressed.len() < content.len());
    assert_eq!(serde_json::from_str::<Value>(compressed).unwrap(), original);
    assert!(!compressed.contains("tokenless retrieve"));
}
