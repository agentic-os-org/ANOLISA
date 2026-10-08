#![cfg(target_os = "linux")]

use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant};

use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};

#[path = "support/fixture.rs"]
mod support;

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

fn run(value: Value, cli: &Path) -> Value {
    let input = encode(value);
    let mut output = Vec::new();
    aw_provider_sec_core::run(
        &mut Cursor::new(&input),
        &mut output,
        cli,
        Path::new("/unused-sec-core.sock"),
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

fn cli_response(value: Value, response: Value) -> Value {
    let directory = support::Directory::new(response);
    run(value, &directory.cli())
}

#[test]
fn code_is_literal_cli_data_and_timeout_is_shared() {
    let directory = support::Directory::new(json!({"stdout":{
        "ok":true,"verdict":"pass","findings":["private evidence"]}}));
    let mut request = invocation();
    let code = "--socket\n$(touch SHOULD_NOT_EXIST); 'quoted'";
    request["event"]["tool"]["input"]["command"] = json!(code);
    request["config"]["tools"]["shell"]["language"] = json!("python");
    let reply = run(request, &directory.cli());
    assert_eq!(reply["effects"][0]["reason_code"], "code_pass");
    let args = directory.argv();
    let args = args.as_array().unwrap();
    assert_eq!(args[0], "--socket");
    assert_eq!(args[1], "/unused-sec-core.sock");
    assert_eq!(args[2], "--timeout-ms");
    let milliseconds: u64 = args[3].as_str().unwrap().parse().unwrap();
    assert!((1..=1000).contains(&milliseconds));
    assert_eq!(
        &args[4..],
        &json!([
            "scan-code",
            "--code",
            code,
            "--language",
            "python",
            "--mode",
            "regex"
        ])
        .as_array()
        .unwrap()[..]
    );
    assert!(!reply.to_string().contains("private evidence"));
    directory.assert_stopped();
}

#[test]
fn host_cancellation_stops_the_nested_scan_cli() {
    use aw_exec::{CommandSpec, Limits};
    use std::sync::atomic::{AtomicBool, Ordering};

    let directory = support::Directory::new(json!({"sleep":30}));
    let mut value = invocation();
    value["budget_ms"] = json!(10000);
    let command = CommandSpec {
        program: env!("CARGO_BIN_EXE_aw-provider-sec-core").into(),
        args: vec![
            "--cli".into(),
            directory.cli().into_os_string(),
            "--socket".into(),
            "/absent.sock".into(),
        ],
        cwd: directory.0.clone(),
        environment: Default::default(),
    };
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            aw_exec::run(
                &command,
                &encode(value),
                Limits {
                    input_bytes: 65536,
                    stdout_bytes: 65536,
                    stderr_bytes: 65536,
                },
                Instant::now() + Duration::from_secs(5),
                &cancelled,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while !directory.0.join("argv.json").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        cancelled.store(true, Ordering::Relaxed);
        assert!(matches!(
            worker.join().unwrap(),
            Err(aw_exec::Error::Cancelled)
        ));
    });
    directory.assert_stopped();
}

#[test]
fn external_deadline_stops_provider_waiting_for_stdin_eof() {
    use aw_exec::{CommandSpec, Limits};
    use std::{ffi::CString, fs::OpenOptions, io::Write, sync::atomic::AtomicBool};

    for input in [b"".as_slice(), b"{"] {
        let directory = support::Directory::new(json!({}));
        let path = directory.0.join("input.fifo");
        let fifo = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the path is a live C string in this test's owned directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        // Linux permits opening a FIFO for both ends without waiting for a peer.
        // Keep the writer open throughout execution so the Provider sees no EOF.
        let mut writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        writer.write_all(input).unwrap();
        let command = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                // Replace the shell in the owned process group. Redirection
                // reproduces a stalled writer outside aw-exec's stdin pipe.
                "exec \"$0\" \"$@\" < \"$AW_STDIN_FIXTURE\"".into(),
                env!("CARGO_BIN_EXE_aw-provider-sec-core").into(),
                "--cli".into(),
                directory.cli().into_os_string(),
                "--socket".into(),
                "/absent.sock".into(),
            ],
            cwd: directory.0.clone(),
            environment: [("AW_STDIN_FIXTURE".into(), path.into_os_string())].into(),
        };
        let started = Instant::now();
        let outcome = aw_exec::run(
            &command,
            &[],
            Limits {
                input_bytes: 0,
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
            started + Duration::from_millis(500),
            &AtomicBool::new(false),
        );
        // DeadlineExceeded is returned only after the executor verifies cleanup.
        assert!(matches!(outcome, Err(aw_exec::Error::DeadlineExceeded)));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            !directory.0.join("cli.pid").exists(),
            "scanner started without EOF"
        );
    }
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
        let reply = cli_response(request, json!({"stdout":{"ok":true,"verdict":verdict}}));
        assert_eq!(reply["status"], "ok");
        assert_eq!(
            reply["effects"],
            json!([{"type":effect,"reason_code":reason}])
        );
        assert!(reply["input_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
    }
}

#[test]
fn failed_or_invalid_scans_never_become_policy_blocks() {
    for (response, code) in [
        (json!({"exit":1,"stderr":"secret"}), "cli_failed"),
        (
            json!({"exit":1,"stdout":{"ok":true,"verdict":"deny"}}),
            "cli_failed",
        ),
        (
            json!({"exit":1,"stdout":{"ok":false,"verdict":"error"}}),
            "cli_failed",
        ),
        (
            json!({"stdout":{"ok":false,"verdict":"error"}}),
            "scan_error",
        ),
        (
            json!({"stdout":{"ok":true,"verdict":"unknown"}}),
            "invalid_scan_result",
        ),
        (
            json!({"stdout":{"ok":false,"verdict":"pass"}}),
            "invalid_scan_result",
        ),
        (json!({"stdout":{"verdict":"warn"}}), "invalid_scan_result"),
        (json!({"raw":"not json"}), "invalid_scan_result"),
        (
            json!({"raw":"{\"ok\":true,\"ok\":false,\"verdict\":\"deny\"}"}),
            "invalid_scan_result",
        ),
        (
            json!({"raw":"{\"ok\":true,\"verdict\":\"pass\"} {}"}),
            "invalid_scan_result",
        ),
        (json!({"stdout_repeat":1048577}), "cli_output_limit"),
        (json!({"stderr_repeat":65537}), "cli_output_limit"),
    ] {
        let reply = cli_response(invocation(), response);
        assert_eq!(reply["error_code"], code);
        assert!(reply.get("effects").is_none());
        assert!(reply.get("input_digest").is_none());
        assert!(!reply.to_string().contains("secret"));
    }
    let reply = run(
        invocation(),
        Path::new("/nonexistent-aw-provider-test.sock"),
    );
    assert_eq!(reply["error_code"], "cli_transport_error");
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
        assert!(aw_provider_sec_core::run(
            &mut Cursor::new(input),
            &mut output,
            Path::new("/absent-cli"),
            Path::new("/absent.sock"),
            Duration::from_secs(1)
        )
        .is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn stalled_cli_respects_invocation_budget_and_reports_failure() {
    let mut request = invocation();
    request["budget_ms"] = json!(200);
    let started = Instant::now();
    let response = cli_response(request, json!({"sleep":2}));
    assert_eq!(response["error_code"], "deadline_exceeded");
    assert!(response.get("effects").is_none());
    assert!(started.elapsed() < Duration::from_secs(2));
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
        let response = cli_response(request, json!({"stdout":{"ok":true,"verdict":"pass"}}));
        assert_eq!(response["effects"][0]["reason_code"], "code_pass");
    }
}
