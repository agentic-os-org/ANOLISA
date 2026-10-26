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
    assert_eq!(args[0], "--trace-context");
    assert_eq!(
        serde_json::from_str::<Value>(args[1].as_str().unwrap()).unwrap(),
        json!({"agent_name":"qoder"})
    );
    assert_eq!(args[2], "--socket");
    assert_eq!(args[3], "/unused-sec-core.sock");
    assert_eq!(args[4], "--timeout-ms");
    let milliseconds: u64 = args[5].as_str().unwrap().parse().unwrap();
    assert!((1..=1000).contains(&milliseconds));
    assert_eq!(
        &args[6..],
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
fn trace_context_keeps_native_ids_across_adapter_dialects() {
    for adapter in [
        "qoder",
        "qoder-sdk",
        "claude-sdk",
        "openclaw",
        "hermes",
        "pi",
        "qwenpaw",
        "agentscope",
    ] {
        let fields = json!({"trace_id":"legacy-trace", "session_id":"session",
            "run_id":"native-run", "call_id":"native-call"});
        let mut native = fields.clone();
        match adapter {
            "openclaw" => {
                native = json!({
                    "event":{"traceId":"legacy-trace", "runId":"native-run",
                        "callId":"native-call", "toolCallId":"native-tool"},
                    "context":{"sessionId":"session", "runId":"other-run",
                        "toolCallId":"other-tool"}
                });
            }
            "hermes" => {
                native = json!({"session_id":"session", "extra":{
                    "traceId":"legacy-trace", "runId":"native-run",
                    "callId":"native-call", "tool_call_id":"native-tool",
                    "api_request_id":"different-api-request"
                }});
            }
            "pi" | "qwenpaw" | "agentscope" => {
                native["tool_call_id"] = json!("different-alias-tool");
                native["tool_call"] = json!({"id":"native-tool"});
                native["invocation_id"] = json!("aw-invocation");
            }
            _ => {
                native["tool_call_id"] = json!("different-alias-tool");
                native["tool_use_id"] = json!("native-tool");
            }
        }
        native["agent_name"] = json!("untrusted-native-label");
        let directory = support::Directory::new(json!({"stdout":{"ok":true,"verdict":"pass"}}));
        let mut request = invocation();
        request["event"]["agent"]["adapter"] = json!(adapter);
        request["event"]["session_id"] = json!("aw-generated-session");
        request["event"]["tool"]["call_id"] = json!("aw-generated-call");
        request["event"]["native"] = native;
        let reply = run(request, &directory.cli());
        assert_eq!(reply["status"], "ok", "{adapter}");
        let args = directory.argv();
        assert_eq!(args[0], "--trace-context", "{adapter}");
        let context: Value = serde_json::from_str(args[1].as_str().unwrap()).unwrap();
        let mut expected = fields;
        expected["agent_name"] = json!(adapter);
        expected["tool_call_id"] = json!("native-tool");
        assert_eq!(context, expected, "{adapter}");
        assert!(reply.get("trace_context").is_none());
        directory.assert_stopped();
    }
}

#[test]
fn trace_context_bounds_native_fields_before_starting_the_cli() {
    for (value, expected) in [
        ("x".repeat(140_000), "x".repeat(242) + "...[truncated]"),
        ("界".repeat(256), "界".repeat(256)),
        ("🦀".repeat(257), "🦀".repeat(242) + "...[truncated]"),
    ] {
        let directory = support::Directory::new(json!({"stdout":{"ok":true,"verdict":"pass"}}));
        let mut request = invocation();
        let padded = format!("\u{1c} \t{value}\n \u{1f}");
        request["event"]["native"] = json!({
            "trace_id":padded,"session_id":padded,"run_id":padded,
            "call_id":padded,"tool_use_id":padded,"tool_call_id":"ignored-alias"
        });
        let reply = run(request, &directory.cli());
        assert_eq!(reply["status"], "ok");
        assert_eq!(reply["effects"][0]["reason_code"], "code_pass");
        let args = directory.argv();
        let context_arg = args[1].as_str().unwrap();
        assert!(context_arg.len() < 8192);
        let context: Value = serde_json::from_str(context_arg).unwrap();
        assert_eq!(
            context,
            json!({
                "agent_name":"qoder","trace_id":expected,"session_id":expected,
                "run_id":expected,"call_id":expected,"tool_call_id":expected
            })
        );
        directory.assert_stopped();
    }
}

#[test]
fn missing_or_invalid_trace_fields_do_not_become_aw_identifiers() {
    for adapter in [
        "qoder",
        "qoder-sdk",
        "claude-sdk",
        "openclaw",
        "hermes",
        "pi",
        "qwenpaw",
        "agentscope",
    ] {
        for native in [
            json!({}),
            json!({"trace_id":null, "session_id":42, "run_id":" \t",
                "call_id":{"id":"invalid"}, "tool_call_id":false,
                "extra":{"api_request_id":"not-a-call-id"},
                "tool_input":{"run_id":"argument-run", "tool_call_id":"argument-tool"}}),
        ] {
            let directory = support::Directory::new(json!({"stdout":{"ok":true,"verdict":"pass"}}));
            let mut request = invocation();
            request["event"]["agent"]["adapter"] = json!(adapter);
            request["event"]["agent"]["instance_id"] = json!("aw-generated-session");
            request["event"]["session_id"] = json!("aw-generated-session");
            request["event"]["tool"]["call_id"] = json!("aw-generated-call");
            request["event"]["native"] = native;
            assert_eq!(run(request, &directory.cli())["status"], "ok", "{adapter}");
            let args = directory.argv();
            assert_eq!(args[0], "--trace-context");
            let context: Value = serde_json::from_str(args[1].as_str().unwrap()).unwrap();
            assert_eq!(context, json!({"agent_name":adapter}), "{adapter}");
            directory.assert_stopped();
        }
    }
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
fn unmapped_tool_configuration_is_explicit_and_validated_offline() {
    for policy in [
        json!("error"),
        json!("observe"),
        json!("block"),
        json!(null),
        json!(true),
    ] {
        let mut config = config();
        config["unmapped_tool"] = policy.clone();
        let reply = run(
            json!({"api_version":VERSION,"method":"validate_config",
                "request_id":"v","config":config}),
            Path::new("/absent-cli"),
        );
        if policy == "error" || policy == "observe" {
            assert_eq!(reply["status"], "ok");
        } else {
            assert_eq!(reply["status"], "error");
            assert_eq!(reply["error_code"], "invalid_config");
        }
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
        for policy in [None, Some("observe")] {
            let mut request = invocation();
            request["config"]["mode"] = json!(mode);
            if let Some(policy) = policy {
                request["config"]["unmapped_tool"] = json!(policy);
            }
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
        assert_eq!(reply["status"], "error");
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
fn unmapped_tools_fail_by_default_and_observation_requires_explicit_policy() {
    let directory = support::Directory::new(json!({"stdout":{"ok":true,"verdict":"pass"}}));
    for mode in ["block", "observe"] {
        for policy in [None, Some("error"), Some("observe")] {
            let mut request = invocation();
            request["config"]["mode"] = json!(mode);
            if mode == "observe" {
                request["allowed_effects"] = json!(["observe"]);
            }
            if let Some(policy) = policy {
                request["config"]["unmapped_tool"] = json!(policy);
            }
            request["event"]["tool"]["name"] = json!("unmapped");
            // Only the normalized name selects a mapping, even if the native
            // name happens to match a configured entry.
            request["event"]["tool"]["native_name"] = json!("shell");
            let reply = run(request, &directory.cli());
            if policy == Some("observe") {
                assert_eq!(reply["status"], "ok");
                assert_eq!(
                    reply["effects"],
                    json!([{
                        "type":"observe","reason_code":"tool_unmapped"
                    }])
                );
                assert!(reply["input_digest"].as_str().is_some());
            } else {
                assert_eq!(reply["status"], "error");
                assert_eq!(reply["error_code"], "tool_unmapped");
                assert!(reply.get("effects").is_none());
                assert!(reply.get("input_digest").is_none());
            }
        }
    }
    assert!(!directory.0.join("cli.pid").exists());
}

#[test]
fn mapping_policy_preserves_mapped_input_and_backend_errors() {
    for policy in [None, Some("observe")] {
        let directory = support::Directory::new(json!({"exit":1,"stderr":"private backend error"}));
        for mode in ["block", "observe"] {
            for input in [
                json!({}),
                json!({"command":null}),
                json!({"command":17}),
                json!({"command":["echo safe"]}),
                json!({"command":{"code":"echo safe"}}),
            ] {
                let mut request = invocation();
                request["config"]["mode"] = json!(mode);
                if let Some(policy) = policy {
                    request["config"]["unmapped_tool"] = json!(policy);
                }
                request["event"]["tool"]["input"] = input;
                let reply = run(request, &directory.cli());
                assert_eq!(reply["status"], "error");
                assert_eq!(reply["error_code"], "invalid_tool_input");
                assert!(reply.get("effects").is_none());
                assert!(!directory.0.join("cli.pid").exists());
            }
        }
        for mode in ["block", "observe"] {
            let mut request = invocation();
            request["config"]["mode"] = json!(mode);
            if let Some(policy) = policy {
                request["config"]["unmapped_tool"] = json!(policy);
            }
            let reply = run(request, &directory.cli());
            assert_eq!(reply["status"], "error");
            assert_eq!(reply["error_code"], "cli_failed");
            assert!(reply.get("effects").is_none());
            assert!(!reply.to_string().contains("private backend error"));
            directory.assert_stopped();
        }
    }
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
