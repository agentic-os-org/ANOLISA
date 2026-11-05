//! PII content transport, fail-closed parsing and tool policy effects.
#![cfg(target_os = "linux")]

#[path = "support/fixture.rs"]
mod support;
use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::{io::Cursor, path::Path, time::Duration};

fn request(output: bool) -> Value {
    json!({"api_version":VERSION,"method":"invoke","request_id":"pii",
        "operation":if output {"scan_tool_output"} else {"scan_tool_input"},
        "config_revision":"a".repeat(64),"budget_ms":1000,
        "allowed_effects":if output {json!(["observe","replace_result"])} else {json!(["observe","block"])},
        "config":{"version":1,"mode":"block","pii":{"input_mode":"block","output_mode":"replace_result"}},
        "event":{"name":if output {"tool.after"} else {"tool.before"},
            "agent":{"adapter":"qwenpaw","binding_id":"target","instance_id":"instance"},
            "session_id":"session","tool":{"name":"read_file","native_name":"read_file","call_id":"call",
                "input":{"path":"safe"},"result":{"content":[{"type":"text","text":"safe"}],"state":"error"},
                "result_projection":{"text":"safe","status":"error","content_origin":"file_content"}},"native":{}}})
}

fn run(mut request: Value, directory: &support::Directory) -> Value {
    if request["event"]["name"] == "tool.before" {
        request["event"]["tool"]["result"] = Value::Null;
        request["event"]["tool"]
            .as_object_mut()
            .unwrap()
            .remove("result_projection");
    }
    let protocol = Protocol::new().unwrap();
    let request = protocol.bind_invocation(request).unwrap();
    let mut output = Vec::new();
    aw_provider_sec_core::run(
        &mut Cursor::new(serde_json::to_vec(request.as_value()).unwrap()),
        &mut output,
        &directory.cli(),
        Path::new("/unused.sock"),
        Duration::from_secs(10),
    )
    .unwrap();
    match protocol.check_response(&request, &output) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => panic!("invalid Provider response: {error}"),
    }
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn tool_input_and_output_distinguish_pass_risk_and_backend_error() {
    for output in [false, true] {
        for sensitive in [false, true] {
            let d = support::Directory::new(json!({"pii":true}));
            let mut req = request(output);
            if sensitive {
                req["event"]["tool"]["input"]["path"] = json!("SENSITIVE");
                req["event"]["tool"]["result"]["content"][0]["text"] = json!("SENSITIVE");
                req["event"]["tool"]["result_projection"]["text"] = json!("SENSITIVE");
            }
            let response = run(req, &d);
            assert_eq!(response["status"], "ok");
            assert_eq!(
                d.argv()[11],
                if output { "tool_output" } else { "tool_input" }
            );
            assert_eq!(
                response["effects"][0]["type"],
                if sensitive {
                    if output {
                        "replace_result"
                    } else {
                        "block"
                    }
                } else {
                    "observe"
                }
            );
            assert!(!response.to_string().contains("SENSITIVE"));
        }
        let d = support::Directory::new(json!({"exit":1}));
        assert_eq!(run(request(output), &d)["error_code"], "cli_failed");
    }
}

#[test]
fn retained_sensitive_fields_and_unsupported_slots_stop_result_delivery() {
    for shape in ["missing", "multiple", "stderr", "metadata"] {
        let d = support::Directory::new(json!({"pii":true}));
        let mut req = request(true);
        req["event"]["tool"]["result"]["content"][0]["text"] = json!("SENSITIVE");
        req["event"]["tool"]["result_projection"]["text"] = json!("SENSITIVE");
        match shape {
            "missing" => {
                req["event"]["tool"]
                    .as_object_mut()
                    .unwrap()
                    .remove("result_projection");
            }
            "multiple" => req["event"]["tool"]["result"]["content"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"text","text":"SENSITIVE"})),
            "stderr" => req["event"]["tool"]["result"]["stderr"] = json!("SENSITIVE other"),
            _ => {
                req["event"]["tool"]["result"]["metadata"] = json!({"credential":"SENSITIVE other"})
            }
        }
        assert_eq!(
            run(req, &d)["error_code"],
            "unsupported_pii_result",
            "{shape}"
        );
    }
}

#[test]
fn qoder_read_projection_allows_replacement_only_with_a_safe_retained_envelope() {
    for retained_secret in [false, true] {
        let d = support::Directory::new(json!({"pii":true}));
        let native = json!({"hook_event_name":"PostToolUse", "tool_name":"Read",
            "tool_input":{"file_path":"file.txt"},
            "tool_response":{"type":"text", "file":{"filePath":"file.txt",
                "content":"SENSITIVE", "numLines":1, "startLine":1, "totalLines":1},
                "metadata":if retained_secret {"SENSITIVE other"} else {"retained"}}});
        let mut req = request(true);
        req["budget_ms"] = json!(5000);
        req["event"]["agent"]["adapter"] = json!("qoder-sdk");
        req["event"]["tool"]["result"] = native["tool_response"].clone();
        req["event"]["tool"]["result_projection"] =
            aw_adapter::projection::project("qoder-sdk", &native).unwrap();
        let response = run(req, &d);
        if retained_secret {
            assert_eq!(response["error_code"], "unsupported_pii_result");
        } else {
            assert_eq!(response["status"], "ok");
            assert_eq!(response["effects"][0]["type"], "replace_result");
        }
        assert!(!response.to_string().contains("SENSITIVE"));
    }
}

#[test]
fn observe_modes_preserve_risk_and_enforcement_requires_admission() {
    for output in [false, true] {
        let d = support::Directory::new(json!({"pii":true}));
        let mut req = request(output);
        req["event"]["tool"]["input"]["path"] = json!("SENSITIVE");
        req["event"]["tool"]["result"] = json!("SENSITIVE");
        req["allowed_effects"] = json!(["observe"]);
        assert_eq!(run(req.clone(), &d)["error_code"], "effect_not_admitted");
        assert!(!d.0.join("cli.pid").exists());
        req["config"]["pii"][if output { "output_mode" } else { "input_mode" }] = json!("observe");
        let response = run(req, &d);
        assert_eq!(
            response["effects"][0],
            json!({"type":"observe","reason_code":"pii_risk"})
        );
    }
}

#[test]
fn decoded_credentials_retain_boundaries_in_arguments_results_and_retained_fields() {
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP";
    // Match the real API-key rule's word boundary: JSON's escaped n/t breaks it.
    let case = json!({"pii":true,"pii_pattern":r"\bsk-[A-Za-z0-9]{48,}\b"});
    for separator in ["\n", "\t", "\r", "\u{0008}", "\u{000c}"] {
        let text = format!("header{separator}{secret}");
        for output in [false, true] {
            let d = support::Directory::new(case.clone());
            let mut req = request(output);
            req["event"]["tool"]["input"] = json!({"nested":[{"command":text}]});
            req["event"]["tool"]["result"]["content"][0]["text"] = json!(text);
            req["event"]["tool"]["result_projection"]["text"] = json!(text);
            let response = run(req, &d);
            assert_eq!(response["status"], "ok", "{response}");
            assert_eq!(
                response["effects"][0]["type"],
                if output { "replace_result" } else { "block" }
            );
            assert!(!response.to_string().contains(secret));
        }
        for retained in ["metadata", "stderr", "key"] {
            let d = support::Directory::new(case.clone());
            let mut req = request(true);
            req["event"]["tool"]["result"]["content"][0]["text"] = json!(secret);
            req["event"]["tool"]["result_projection"]["text"] = json!(secret);
            if retained == "key" {
                req["event"]["tool"]["result"]["metadata"] = json!({text.clone():"safe"});
            } else {
                req["event"]["tool"]["result"][retained] = json!({"note":[text]});
            }
            assert_eq!(
                run(req, &d)["error_code"],
                "unsupported_pii_result",
                "{retained}"
            );
        }
    }
}

#[test]
fn decoded_strings_and_structured_field_context_are_both_scanned() {
    for output in [false, true] {
        for (value, pattern) in [
            (json!("secret\n\tvalue"), r"\Asecret\n\tvalue\Z"),
            (
                json!({"nested":["secret\n\tvalue"]}),
                r"\Asecret\n\tvalue\Z",
            ),
            (
                json!({"password":"abcdefghijklmnop"}),
                r#""password":"abcdefghijklmnop""#,
            ),
            (json!({"phone":13800138000_u64}), r"13800138000"),
        ] {
            let d = support::Directory::new(json!({"pii":true,"pii_pattern":pattern}));
            let mut req = request(output);
            req["config"]["pii"][if output { "output_mode" } else { "input_mode" }] =
                json!("observe");
            req["event"]["tool"][if output { "result" } else { "input" }] = value;
            let response = run(req, &d);
            assert_eq!(
                response["effects"][0],
                json!({"type":"observe","reason_code":"pii_risk"})
            );
        }
    }
}

#[test]
fn decoded_scans_and_retained_checks_share_one_deadline() {
    for sensitive in [false, true] {
        let d = support::Directory::new(json!({"pii":true,"sleep":0.06}));
        let mut req = request(true);
        if sensitive {
            req["event"]["tool"]["result"]["content"][0]["text"] = json!("SENSITIVE");
            req["event"]["tool"]["result_projection"]["text"] = json!("SENSITIVE");
        }
        // Leave startup headroom; budget sharing is checked in the CLI arguments.
        req["budget_ms"] = json!(5000);
        let response = run(req.clone(), &d);
        assert_eq!(response["status"], "ok", "{response}");
        let scans: Vec<Value> = std::fs::read_to_string(d.0.join("scans.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert!(scans.len() > 2);
        for pair in scans.windows(2) {
            let budget = |s: &Value| s["argv"][5].as_str().unwrap().parse::<u64>().unwrap();
            assert!(budget(&pair[1]) < budget(&pair[0]));
        }
        let d = support::Directory::new(json!({"pii":true,"sleep":0.15}));
        req["budget_ms"] = json!(250);
        let response = run(req, &d);
        assert_eq!(response["error_code"], "deadline_exceeded");
        d.assert_stopped();
    }
}
