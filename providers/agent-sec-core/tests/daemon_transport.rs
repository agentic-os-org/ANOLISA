//! Direct bounded daemon client transport tests (strict V2 wire contract, peer UID auth, parity, trace propagation, no retry).

#[path = "support/daemon.rs"]
mod support;

use aw_provider::VERSION;
use aw_provider_sec_core::{
    daemon_client::{
        build_request, decode_response, DaemonError, DaemonRequest, ErrorResponse, SuccessResponse,
        COMPATIBILITY_VERSION_V1, TRACE_CARRIER_VERSION_V1,
    },
    run_with_transport, Transport,
};
use serde_json::{json, Value};
#[cfg(feature = "testing")]
use std::io::{BufRead, BufReader, Write};
#[cfg(feature = "testing")]
use std::sync::{atomic::AtomicBool, Mutex};
use std::{
    fs,
    io::Read,
    os::unix::net::UnixListener,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
#[cfg(feature = "testing")]
use support::{
    check_final_tool_output_pii_request, scan_prompt_request, scan_tool_input_pii_request,
    scan_tool_output_pii_request, scan_user_pii_request,
};
use support::{scan_code_request, temp_dir};

#[test]
fn daemon_request_wire_contract_forbids_client_request_id_and_requires_v1_carriers() {
    let trace_ctx = json!({
        "agent_name": "qoder",
        "session_id": "sess-1",
        "run_id": "run-1",
        "tool_call_id": "call-1",
        "trace_id": "legacy-trace-1",
        "invocation_label": "hook-1"
    });
    let req = build_request("action.code_scan", json!({"code": "print(1)"}), &trace_ctx)
        .expect("build request");
    let wire: Value = serde_json::to_value(&req).expect("serialize");
    assert!(
        wire.get("requestId").is_none(),
        "DaemonRequest must not include client requestId"
    );
    assert_eq!(wire["method"], "action.code_scan");
    assert_eq!(wire["traceContext"]["version"], TRACE_CARRIER_VERSION_V1);
    assert!(wire["traceContext"]["baggage"]
        .as_str()
        .unwrap()
        .contains("agentsec.agent.name=qoder"));
    assert_eq!(wire["compatibility"]["version"], COMPATIBILITY_VERSION_V1);
    assert_eq!(wire["compatibility"]["traceId"], "legacy-trace-1");

    // Strict deserialization rejects client requestId or any unknown field.
    let with_request_id = json!({
        "requestId": "client-123",
        "method": "action.code_scan",
        "params": {}
    });
    assert!(
        serde_json::from_value::<DaemonRequest>(with_request_id).is_err(),
        "DaemonRequest must reject unknown field requestId"
    );

    // decode_response accepts any non-empty server-generated requestId.
    let ok_bytes = serde_json::to_vec(&SuccessResponse {
        request_id: "daemon-uuid-999".to_owned(),
        result: json!({"ok": true, "verdict": "pass"}),
    })
    .unwrap();
    let decoded = decode_response(&ok_bytes).expect("decode server response");
    assert_eq!(decoded["verdict"], "pass");

    let err_bytes = serde_json::to_vec(&ErrorResponse {
        request_id: "daemon-uuid-err".to_owned(),
        error: DaemonError {
            code: "SCAN_FAILED".to_owned(),
            message: "backend error".to_owned(),
        },
    })
    .unwrap();
    assert_eq!(decode_response(&err_bytes), Err("daemon_failed"));
}

#[test]
fn daemon_only_describe_omits_check_skill_until_cli_is_supplied() {
    let raw = serde_json::to_vec(&json!({
        "api_version": VERSION,
        "method": "describe",
        "request_id": "desc-1"
    }))
    .unwrap();

    let mut input = raw.as_slice();
    let mut out_no_cli = Vec::new();
    run_with_transport(
        &mut input,
        &mut out_no_cli,
        Path::new(""),
        Path::new("/tmp/nonexistent.sock"),
        Duration::from_secs(1),
        false,
        Transport::Daemon,
    )
    .unwrap();
    let desc_no_cli: Value = serde_json::from_slice(&out_no_cli).unwrap();
    let ops_no_cli: Vec<&str> = desc_no_cli["operations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|op| op["name"].as_str())
        .collect();
    assert!(!ops_no_cli.contains(&"check_skill"));
    assert!(ops_no_cli.contains(&"scan_code"));
    assert!(ops_no_cli.contains(&"scan_prompt"));
    assert!(ops_no_cli.contains(&"scan_user_input"));
    assert!(ops_no_cli.contains(&"scan_tool_input"));
    assert!(ops_no_cli.contains(&"scan_tool_output"));
    assert!(ops_no_cli.contains(&"check_final_tool_output"));

    let mut input2 = raw.as_slice();
    let mut out_with_cli = Vec::new();
    run_with_transport(
        &mut input2,
        &mut out_with_cli,
        Path::new("/usr/bin/agent-sec-cli"),
        Path::new("/tmp/nonexistent.sock"),
        Duration::from_secs(1),
        false,
        Transport::Daemon,
    )
    .unwrap();
    let desc_with_cli: Value = serde_json::from_slice(&out_with_cli).unwrap();
    let ops_with_cli: Vec<&str> = desc_with_cli["operations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|op| op["name"].as_str())
        .collect();
    assert!(ops_with_cli.contains(&"check_skill"));
}

#[test]
fn non_root_daemon_is_rejected_before_writing_request() {
    let dir = temp_dir("uid");
    let sock = dir.join("daemon.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let bytes_seen = Arc::new(AtomicUsize::new(0));
    let seen_clone = Arc::clone(&bytes_seen);
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buf = [0u8; 64];
            if let Ok(n) = stream.read(&mut buf) {
                seen_clone.fetch_add(n, Ordering::Relaxed);
            }
        }
    });

    let req = serde_json::to_vec(&scan_code_request("echo ok", 2000)).unwrap();
    let mut input = req.as_slice();
    let mut output = Vec::new();
    run_with_transport(
        &mut input,
        &mut output,
        Path::new(""),
        &sock,
        Duration::from_secs(5),
        false,
        Transport::Daemon,
    )
    .unwrap();
    let reply: Value = serde_json::from_slice(&output).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["error_code"], "daemon_peer_auth_failed");
        handle.join().unwrap();
        assert_eq!(bytes_seen.load(Ordering::Relaxed), 0);
    }
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(feature = "testing")]
fn invoke_binary_daemon(socket: &Path, request: &Value) -> Value {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_aw-provider-sec-core"));
    cmd.args(["--transport", "daemon", "--socket"])
        .arg(socket)
        .env("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[cfg(feature = "testing")]
#[test]
fn daemon_transport_parity_trace_and_zero_cli_spawn() {
    let dir = temp_dir("parity");
    let sock = dir.join("daemon.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let connections = Arc::new(AtomicUsize::new(0));
    let recorded: Arc<Mutex<Vec<DaemonRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let mode = Arc::new(Mutex::new("normal".to_string()));

    let srv = thread::spawn({
        let stop = Arc::clone(&stop);
        let connections = Arc::clone(&connections);
        let recorded = Arc::clone(&recorded);
        let mode = Arc::clone(&mode);
        move || {
            while !stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let seq = connections.fetch_add(1, Ordering::Relaxed) + 1;
                stream.set_nonblocking(false).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let raw: Value = serde_json::from_str(&line).expect("valid JSON line");
                assert!(
                    raw.get("requestId").is_none(),
                    "V2 DaemonRequest must not contain client requestId: {raw}"
                );
                let req: DaemonRequest =
                    serde_json::from_str(&line).expect("strict V2 DaemonRequest decode");
                if let Some(tc) = &req.trace_context {
                    assert_eq!(tc.version, TRACE_CARRIER_VERSION_V1);
                }
                if let Some(cm) = &req.compatibility {
                    assert_eq!(cm.version, COMPATIBILITY_VERSION_V1);
                }
                recorded.lock().unwrap().push(req.clone());
                let current_mode = mode.lock().unwrap().clone();
                if current_mode == "sleep" {
                    thread::sleep(Duration::from_millis(400));
                    continue;
                }
                let srv_req_id = format!("daemon-srv-{seq}");
                let resp = if current_mode == "error" {
                    serde_json::to_value(ErrorResponse {
                        request_id: srv_req_id,
                        error: DaemonError {
                            code: "backend_down".to_owned(),
                            message: "boom".to_owned(),
                        },
                    })
                    .unwrap()
                } else {
                    let result = match req.method.as_str() {
                        "action.code_scan" => {
                            let code = req.params["code"].as_str().unwrap();
                            let verdict = if code.contains("rm -rf") {
                                "deny"
                            } else {
                                "pass"
                            };
                            json!({"ok": true, "verdict": verdict})
                        }
                        "action.prompt_scan" => {
                            let text = req.params["text"].as_str().unwrap();
                            let trimmed = text.trim();
                            let risky = text.contains("INJECT");
                            json!({
                                "schema_version": "1.0",
                                "ok": !risky,
                                "verdict": if risky { "deny" } else { "pass" },
                                "input_truncated": false,
                                "input_bytes_scanned": trimmed.len(),
                                "degraded": false,
                                "layers_failed": [],
                                "layer_results": [{"layer": "rule_engine"}]
                            })
                        }
                        "action.pii_scan" => {
                            let text = req.params["text"].as_str().unwrap();
                            let risky = text.contains("SENSITIVE");
                            json!({
                                "ok": true,
                                "verdict": if risky { "deny" } else { "pass" },
                                "summary": {
                                    "source": req.params["source"],
                                    "truncated": false,
                                    "bytes_scanned": text.len()
                                }
                            })
                        }
                        _ => unreachable!(),
                    };
                    serde_json::to_value(SuccessResponse {
                        request_id: srv_req_id,
                        result,
                    })
                    .unwrap()
                };
                writeln!(stream, "{resp}").unwrap();
            }
        }
    });

    // 1. scan_code pass & block + traceContext/compatibility
    let pass_code = invoke_binary_daemon(&sock, &scan_code_request("echo safe", 2000));
    assert_eq!(pass_code["status"], "ok");
    assert_eq!(
        pass_code["effects"],
        json!([{"type": "observe", "reason_code": "code_pass"}])
    );

    let deny_code = invoke_binary_daemon(&sock, &scan_code_request("rm -rf /tmp/x", 2000));
    assert_eq!(deny_code["status"], "ok");
    assert_eq!(
        deny_code["effects"],
        json!([{"type": "block", "reason_code": "code_risk"}])
    );

    // 2. scan_prompt pass, whitespace skip, and block
    let pass_prompt = invoke_binary_daemon(&sock, &scan_prompt_request("hello", 2000));
    assert_eq!(
        pass_prompt["effects"],
        json!([{"type": "observe", "reason_code": "prompt_pass"}])
    );
    let empty_prompt = invoke_binary_daemon(&sock, &scan_prompt_request("  \n\t ", 2000));
    assert_eq!(
        empty_prompt["effects"],
        json!([{"type": "observe", "reason_code": "prompt_pass"}])
    );
    let deny_prompt = invoke_binary_daemon(&sock, &scan_prompt_request("INJECT", 2000));
    assert_eq!(
        deny_prompt["effects"],
        json!([{"type": "block", "reason_code": "prompt_risk"}])
    );

    // 3. scan_user_input pass & block
    let pass_pii = invoke_binary_daemon(&sock, &scan_user_pii_request("public", 2000));
    assert_eq!(
        pass_pii["effects"],
        json!([{"type": "observe", "reason_code": "pii_pass"}])
    );
    let deny_pii = invoke_binary_daemon(&sock, &scan_user_pii_request("my SENSITIVE token", 2000));
    assert_eq!(
        deny_pii["effects"],
        json!([{"type": "block", "reason_code": "pii_risk"}])
    );

    // 4. scan_tool_input and scan_tool_output over daemon without --cli
    let deny_tool_in = invoke_binary_daemon(
        &sock,
        &scan_tool_input_pii_request("curl -H SENSITIVE", 2000),
    );
    assert_eq!(deny_tool_in["status"], "ok");
    assert_eq!(deny_tool_in["effects"][0]["type"], "block");
    assert_eq!(deny_tool_in["effects"][0]["reason_code"], "pii_risk");

    let replace_tool_out = invoke_binary_daemon(
        &sock,
        &scan_tool_output_pii_request("secret SENSITIVE value", 2000),
    );
    assert_eq!(replace_tool_out["status"], "ok");
    assert_eq!(replace_tool_out["effects"][0]["type"], "replace_result");

    let deny_final = invoke_binary_daemon(
        &sock,
        &check_final_tool_output_pii_request("final SENSITIVE value", 2000),
    );
    assert_eq!(deny_final["status"], "ok");
    assert_eq!(deny_final["effects"][0]["type"], "block");
    assert_eq!(deny_final["effects"][0]["reason_code"], "pii_risk");

    // 5. Verify traceContext.version == 1, baggage, and compatibility on scan_code call
    let calls = recorded.lock().unwrap().clone();
    assert!(!calls.is_empty());
    let first = &calls[0];
    let compat = first.compatibility.as_ref().expect("compatibility present");
    assert_eq!(compat.version, 1);
    assert_eq!(compat.trace_id.as_deref(), Some("trace-abc-123"));
    let tc = first.trace_context.as_ref().expect("traceContext present");
    assert_eq!(tc.version, 1);
    let baggage = tc.baggage.as_deref().unwrap();
    assert!(baggage.contains("agentsec.agent.name=qoder"));
    assert!(baggage.contains("agentsec.session.id=sess-1"));
    assert!(baggage.contains("agentsec.tool_call.id=native-tool-9"));

    // 6. Daemon error & timeout never retry and never fall back to CLI
    *mode.lock().unwrap() = "error".to_string();
    let count_before_err = connections.load(Ordering::Relaxed);
    let err_reply = invoke_binary_daemon(&sock, &scan_code_request("echo ok", 2000));
    assert_eq!(err_reply["status"], "error");
    assert_eq!(err_reply["error_code"], "daemon_failed");
    assert_eq!(connections.load(Ordering::Relaxed), count_before_err + 1);

    *mode.lock().unwrap() = "sleep".to_string();
    let count_before_timeout = connections.load(Ordering::Relaxed);
    let timeout_reply = invoke_binary_daemon(&sock, &scan_code_request("echo ok", 100));
    assert_eq!(timeout_reply["status"], "error");
    assert_eq!(timeout_reply["error_code"], "deadline_exceeded");
    assert_eq!(
        connections.load(Ordering::Relaxed),
        count_before_timeout + 1
    );

    stop.store(true, Ordering::Release);
    srv.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn daemon_client_enforces_independent_business_and_context_frame_budgets() {
    use aw_provider_sec_core::daemon_client::{build_request, encode_bounded_request};

    // Envelope overhead (`{"method":"action.code_scan","params":` + `}` + LF) pushes a
    // 4 MiB - 20 byte params object over MAX_BUSINESS_BYTES (4 MiB) even though total
    // length stays well below MAX_REQUEST_FRAME_BYTES (4 MiB + 32 KiB).
    let max_business = 4 * 1024 * 1024;
    let params = json!({"code": "a".repeat(max_business - 20)});
    assert!(serde_json::to_vec(&params).unwrap().len() <= max_business);
    let req = build_request("action.code_scan", params, &json!({})).unwrap();
    assert_eq!(
        encode_bounded_request(&req).unwrap_err(),
        "daemon_frame_limit"
    );

    // Context budget (`traceContext` + `compatibility` <= 32 KiB) is checked together, not individually.
    let max_ctx = 32 * 1024;
    let big_trace = json!({
        "traceparent": "x".repeat(max_ctx - 512),
        "trace_id": "y".repeat(600),
    });
    let err = build_request("action.code_scan", json!({"code": "ok"}), &big_trace)
        .and_then(|r| encode_bounded_request(&r))
        .unwrap_err();
    assert_eq!(err, "daemon_frame_limit");
}

#[test]
fn daemon_client_rejects_unsupported_and_intent_methods() {
    use aw_provider_sec_core::daemon_client::{
        build_request, encode_bounded_request, DaemonRequest,
    };

    for method in [
        "intent.propose",
        "intent.propose_command",
        "daemon.health",
        "",
    ] {
        assert_eq!(
            build_request(method, json!({"text": "x"}), &json!({})).unwrap_err(),
            "invalid_daemon_request"
        );
        let crafted = DaemonRequest {
            method: method.to_owned(),
            params: json!({"text": "x"}),
            trace_context: None,
            compatibility: None,
        };
        assert_eq!(
            encode_bounded_request(&crafted).unwrap_err(),
            "invalid_daemon_request"
        );
    }
}
