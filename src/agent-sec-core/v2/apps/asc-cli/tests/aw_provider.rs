use std::io::{BufRead, BufReader, Cursor, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

use aw_provider::{Protocol, VERSION};
use serde_json::{Value, json};

mod common;

fn config() -> Value {
    json!({"version":1,"mode":"block","tools":{
        "shell":{"language":"bash","input_pointer":"/command"}
    }})
}

fn invocation() -> Value {
    json!({
        "api_version":VERSION,"method":"invoke","request_id":"invoke-1",
        "operation":"scan_code","config_revision":"b".repeat(64),
        "budget_ms":1000,"allowed_effects":["observe","block"],"config":config(),
        "event":{"name":"tool.before",
            "agent":{"adapter":"qoder","binding_id":"target","instance_id":"instance"},
            "session_id":"session","tool":{"name":"shell","native_name":"Bash",
                "call_id":"call","input":{"command":"echo safe"},"result":null},"native":{}}
    })
}

fn encode(value: Value) -> Vec<u8> {
    let value = if value["method"] == "invoke" {
        Protocol::new()
            .unwrap()
            .bind_invocation(value)
            .unwrap()
            .as_value()
            .clone()
    } else {
        value
    };
    serde_json::to_vec(&value).unwrap()
}

fn run(value: Value, socket: &Path) -> Value {
    let input = encode(value);
    let mut output = Vec::new();
    asc_cli::provider::run(
        &mut Cursor::new(&input),
        &mut output,
        socket,
        Duration::from_secs(2),
    )
    .unwrap();
    let protocol = Protocol::new().unwrap();
    let request = protocol.parse_request(&input).unwrap();
    match protocol.check_response(&request, &output) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => panic!("response violated protocol: {error}"),
    }
    serde_json::from_slice(&output).unwrap()
}

fn daemon_response(value: Value, response: Value) -> Value {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "provider did not connect");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "action.code_scan");
        assert_eq!(request["params"]["code"], "echo safe");
        assert_eq!(request["params"]["language"], "bash");
        assert_eq!(request["params"]["mode"], "regex");
        assert!(request["params"]["rules"].is_null());
        if response.is_null() {
            std::thread::sleep(Duration::from_millis(100));
        } else {
            writeln!(stream, "{response}").unwrap();
        }
    });
    let reply = run(value, &socket);
    worker.join().unwrap();
    reply
}

#[test]
fn offline_methods_and_after_observation_do_not_need_the_daemon() {
    let socket = Path::new("/nonexistent-aw-provider-test.sock");
    let description = run(
        json!({"api_version":VERSION,"method":"describe","request_id":"d"}),
        socket,
    );
    assert_eq!(
        description["operations"],
        json!([
            {"name":"scan_code","events":["tool.before"],"effects":["observe","block"]},
            {"name":"observe_tool","events":["tool.after"],"effects":["observe"]}
        ])
    );
    let validated = run(
        json!({"api_version":VERSION,"method":"validate_config",
        "request_id":"v","config":config()}),
        socket,
    );
    assert_eq!(validated["status"], "ok");
    let mut request = invocation();
    request["operation"] = json!("observe_tool");
    request["event"]["name"] = json!("tool.after");
    request["event"]["tool"]["result"] = json!({"secret":"not returned"});
    request["allowed_effects"] = json!(["observe"]);
    let reply = run(request, socket);
    assert_eq!(
        reply["effects"],
        json!([{"type":"observe","reason_code":"tool_observed"}])
    );
    assert!(!reply.to_string().contains("secret"));
}

#[test]
fn private_configuration_rejects_typos_and_ambiguous_mappings() {
    for invalid in [
        json!({}),
        json!({"version":2,"mode":"observe","tools":{}}),
        json!({"version":1,"mode":"ask","tools":{}}),
        json!({"version":1,"mode":"observe","tools":{}}),
        json!({"version":1,"mode":"observe","unknown":true,"tools":{
            "shell":{"language":"bash","input_pointer":"/command"}}}),
        json!({"version":1,"mode":"observe","tools":{
            "shell":{"language":"ruby","input_pointer":"/command"}}}),
        json!({"version":1,"mode":"observe","tools":{
            "shell":{"language":"bash","input_pointer":"command"}}}),
        json!({"version":1,"mode":"observe","tools":{
            "shell":{"language":"bash","input_pointer":"/bad~escape"}}}),
        json!({"version":1,"mode":"observe","tools":{
            " ":{"language":"bash","input_pointer":"/command"}}}),
    ] {
        let reply = run(
            json!({"api_version":VERSION,"method":"validate_config",
            "request_id":"v","config":invalid}),
            Path::new("/absent.sock"),
        );
        assert_eq!(reply["error_code"], "invalid_config");
    }
}

#[test]
fn risk_is_a_successful_block_only_with_explicit_mode_and_admission() {
    for (mode, verdict, effect, reason) in [
        ("block", "pass", "observe", "code_pass"),
        ("block", "warn", "block", "code_risk"),
        ("block", "deny", "block", "code_risk"),
        ("observe", "warn", "observe", "code_risk"),
    ] {
        let mut request = invocation();
        request["config"]["mode"] = json!(mode);
        let reply = daemon_response(
            request,
            json!({"requestId":"scan",
            "result":{"ok":true,"verdict":verdict}}),
        );
        assert_eq!(reply["status"], "ok");
        assert_eq!(
            reply["effects"],
            json!([{"type":effect,"reason_code":reason}])
        );
        assert!(
            reply["input_digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }
}

#[test]
fn failed_or_invalid_scans_never_become_policy_blocks() {
    for (response, code) in [
        (
            json!({"requestId":"s","error":{"code":"internal","message":"secret"}}),
            "daemon_error",
        ),
        (
            json!({"requestId":"s","result":{"ok":false,"verdict":"error"}}),
            "scan_error",
        ),
        (
            json!({"requestId":"s","result":{"ok":true,"verdict":"unknown"}}),
            "invalid_scan_result",
        ),
        (
            json!({"requestId":"s","result":{"ok":false,"verdict":"pass"}}),
            "invalid_scan_result",
        ),
        (
            json!({"requestId":"s","result":{"verdict":"warn"}}),
            "invalid_scan_result",
        ),
    ] {
        let reply = daemon_response(invocation(), response);
        assert_eq!(reply["error_code"], code);
        assert!(reply.get("effects").is_none());
        assert!(reply.get("input_digest").is_none());
        assert!(!reply.to_string().contains("secret"));
    }
    let reply = run(
        invocation(),
        Path::new("/nonexistent-aw-provider-test.sock"),
    );
    assert_eq!(reply["error_code"], "daemon_transport_error");
}

#[test]
fn selection_and_admission_failures_are_not_silent_scan_successes() {
    let socket = Path::new("/absent.sock");
    let mut request = invocation();
    request["event"]["tool"]["name"] = json!("unmapped");
    assert_eq!(
        run(request, socket)["effects"][0]["reason_code"],
        "tool_unmapped"
    );
    let mut request = invocation();
    request["event"]["tool"]["input"] = json!({"different": "echo safe"});
    assert_eq!(run(request, socket)["error_code"], "invalid_tool_input");
    let mut request = invocation();
    request["allowed_effects"] = json!(["observe"]);
    assert_eq!(run(request, socket)["error_code"], "block_not_admitted");
    let mut request = invocation();
    request["operation"] = json!("unknown");
    assert_eq!(
        run(request, socket)["error_code"],
        "unsupported_operation_event"
    );
}

#[test]
fn malformed_transport_is_rejected_before_any_response() {
    let mut bound: Value = serde_json::from_slice(&encode(invocation())).unwrap();
    bound["event"]["tool"]["input"]["command"] = json!("tampered");
    let mut cases = vec![
        serde_json::to_vec(&bound).unwrap(),
        br#"{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"a","request_id":"b"}"#.to_vec(),
        b"not JSON".to_vec(),
        vec![b' '; aw_provider::MAX_MESSAGE_BYTES + 1],
    ];
    let mut deep = json!(null);
    for _ in 0..=aw_provider::MAX_DEPTH {
        deep = json!([deep]);
    }
    cases.push(
        serde_json::to_vec(&json!({"api_version":VERSION,"method":"validate_config",
        "request_id":"v","config":{"nested":deep}}))
        .unwrap(),
    );
    for input in cases {
        let mut output = Vec::new();
        assert!(
            asc_cli::provider::run(
                &mut Cursor::new(input),
                &mut output,
                Path::new("/absent.sock"),
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(output.is_empty());
    }
}

#[test]
fn stalled_daemon_respects_invocation_budget_and_reports_failure() {
    let mut request = invocation();
    request["budget_ms"] = json!(50);
    let started = Instant::now();
    let response = daemon_response(request, Value::Null);
    assert_eq!(response["error_code"], "deadline_exceeded");
    assert!(response.get("effects").is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn json_pointer_supports_arbitrary_names_nested_values_and_root_strings() {
    for (pointer, input) in [
        ("/a~1b/~0key", json!({"a/b":{"~key":"echo safe"}})),
        ("", json!("echo safe")),
        ("/items/0", json!({"items":["echo safe"]})),
    ] {
        let mut request = invocation();
        request["config"]["tools"]["shell"]["input_pointer"] = json!(pointer);
        request["event"]["tool"]["input"] = input;
        let response = daemon_response(
            request,
            json!({"requestId":"s",
            "result":{"ok":true,"verdict":"pass"}}),
        );
        assert_eq!(response["effects"][0]["reason_code"], "code_pass");
    }
}
