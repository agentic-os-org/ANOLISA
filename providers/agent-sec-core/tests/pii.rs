//! PII content transport, fail-closed parsing and tool policy effects.
#![cfg(target_os = "linux")]

#[path = "pii/ruleset.rs"]
mod ruleset;
#[path = "support/fixture.rs"]
mod support;
use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::{io::Cursor, path::Path, time::Duration};

fn request(output: bool) -> Value {
    json!({"api_version":VERSION,"method":"invoke","request_id":"pii",
        "operation":if output {"scan_tool_output"} else {"scan_tool_input"},
        "config_revision":"a".repeat(64),"budget_ms":5000,
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
            let d = support::Directory::new(json!({"pii":true,"v1_ruleset_sha256":"a".repeat(64)}));
            let mut req = request(output);
            if !output {
                req["config"]["pii"]["output_mode"] = json!("mask");
            }
            if sensitive {
                req["event"]["tool"]["input"]["path"] = json!("SENSITIVE");
                req["event"]["tool"]["result"]["content"][0]["text"] = json!("SENSITIVE");
                req["event"]["tool"]["result_projection"]["text"] = json!("SENSITIVE");
            }
            let response = run(req, &d);
            assert_eq!(response["status"], "ok", "{response}");
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
fn final_tool_output_blocks_sensitive_finalized_content() {
    for sensitive in [false, true] {
        let d = support::Directory::new(json!({"pii":true,"v1_ruleset_sha256":"a".repeat(64)}));
        let mut req = request(true);
        req["config"]["pii"]["output_mode"] = json!("mask");
        req["operation"] = json!("check_final_tool_output");
        req["allowed_effects"] = json!(["observe", "block"]);
        req["event"]["name"] = json!("security.violation");
        req["event"]["agent"]["adapter"] = json!("deepseek-harness");
        if sensitive {
            req["event"]["tool"]["result"]["content"][0]["text"] = json!("SENSITIVE");
        }
        let response = run(req, &d);
        assert_eq!(response["status"], "ok");
        assert_eq!(
            response["effects"][0]["type"],
            if sensitive { "block" } else { "observe" }
        );
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
fn codex_whole_mcp_projection_replaces_serialized_result() {
    let d = support::Directory::new(json!({"pii":true}));
    let native = json!({"tool_name":"mcp__fixture__echo",
        "tool_input":{"value":"safe"},
        "tool_response":{"content":[{"type":"text","text":"SENSITIVE"}],
            "structuredContent":{"private":"SENSITIVE"},"isError":false}});
    let mut req = request(true);
    req["event"]["agent"]["adapter"] = json!("codex");
    req["event"]["tool"]["result"] = native["tool_response"].clone();
    req["event"]["tool"]["result_projection"] =
        aw_adapter::projection::project("codex", &native).unwrap();
    let response = run(req.clone(), &d);
    assert_eq!(response["status"], "ok", "{response}");
    assert_eq!(response["effects"][0]["type"], "replace_result");
    assert!(!response.to_string().contains("SENSITIVE"));

    req["event"]["tool"]["result_projection"]["text"] = json!("SENSITIVE");
    assert_eq!(run(req, &d)["error_code"], "unsupported_pii_result");
}

#[test]
fn observe_modes_preserve_risk_and_enforcement_requires_admission() {
    for output in [false, true] {
        let d = support::Directory::new(json!({"pii":true,"v1_ruleset_sha256":"a".repeat(64)}));
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

const SLOT: &str = "contact SENSITIVE for delivery";
const WITHHELD: &str = "Sensitive data detected by AW; content withheld.";

fn mask_request(text: &str) -> Value {
    let mut req = request(true);
    req["config"]["pii"]["output_mode"] = json!("mask");
    req["event"]["tool"]["result"] = json!(text);
    req["event"]["tool"]["result_projection"]["text"] = json!(text);
    req
}

fn scans(d: &support::Directory) -> Vec<Value> {
    std::fs::read_to_string(d.0.join("scans.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn mask_checks_the_risky_slot_and_its_original_json_after_envelope_checks() {
    let d = support::Directory::new(json!({"pii":true}));
    let safe = run(mask_request("public"), &d);
    assert_eq!(
        safe["effects"],
        json!([{"type":"observe","reason_code":"pii_pass"}])
    );
    let calls = scans(&d);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["argv"].as_array().unwrap().len(), 12);

    let mut req = mask_request(SLOT);
    req["event"]["tool"]["result"] = json!({"stdout":SLOT, "stderr":"ok"});
    let d = support::Directory::new(json!({"pii":true}));
    let response = run(req.clone(), &d);
    assert_eq!(
        response["effects"],
        json!([{"type":"replace_result","reason_code":"pii_risk",
        "text":"contact [MASKED] for delivery"}])
    );
    let calls = scans(&d);
    let (review, earlier) = calls.split_last().unwrap();
    assert_eq!(review["argv"].as_array().unwrap().len(), 12);
    assert_eq!(
        serde_json::from_str::<Value>(review["text"].as_str().unwrap()).unwrap(),
        json!({"stderr":"ok","stdout":"contact [MASKED] for delivery"})
    );
    let (original, earlier) = earlier.split_last().unwrap();
    assert_eq!(original["text"], req["event"]["tool"]["result"].to_string());
    assert_eq!(original["argv"][12], "--redact-output");
    let (candidate, earlier) = earlier.split_last().unwrap();
    assert_eq!(candidate["text"], "contact [MASKED] for delivery");
    assert_eq!(candidate["argv"].as_array().unwrap().len(), 12);
    let (last, checks) = earlier.split_last().unwrap();
    assert!(checks.iter().any(|c| c["text"] == "ok"));
    assert!(checks
        .iter()
        .all(|c| c["argv"].as_array().unwrap().len() == 12));
    assert_eq!(last["argv"][12], "--redact-output");
    assert_eq!(last["argv"][1], checks[0]["argv"][1]);
    let budget = |c: &Value| c["argv"][5].as_str().unwrap().parse::<u64>().unwrap();
    assert!(budget(last) < budget(&checks[0]));
    assert_eq!(last["text"], SLOT);

    let d = support::Directory::new(json!({"pii":true}));
    req["event"]["tool"]["result"]["stderr"] = json!("SENSITIVE stderr");
    assert_eq!(run(req.clone(), &d)["error_code"], "unsupported_pii_result");
    assert!(scans(&d)
        .iter()
        .all(|c| c["argv"].as_array().unwrap().len() == 12));

    let d = support::Directory::new(json!({"pii":true}));
    req["allowed_effects"] = json!(["observe"]);
    assert_eq!(run(req, &d)["error_code"], "effect_not_admitted");
    assert!(!d.0.join("scans.jsonl").exists());
}

#[test]
fn mask_withholds_a_second_risk_visible_only_in_field_context() {
    let slot = "account and SENSITIVE";
    let mut req = mask_request(slot);
    req["event"]["tool"]["result"] = json!({"stdout":slot});
    for pattern in [
        r#"SENSITIVE|(?<="stdout":")account"#,
        r#"SENSITIVE|(?<="stdout":")account(?= and \[MASKED\])"#,
    ] {
        let d = support::Directory::new(json!({"pii":true,"pii_pattern":pattern}));
        assert_eq!(
            run(req.clone(), &d)["effects"],
            json!([{"type":"replace_result","reason_code":"pii_risk","text":WITHHELD}])
        );
    }
}

#[test]
fn original_field_context_cannot_be_erased_by_masking_the_slot_marker() {
    let slot = "MARKER customer-123";
    let mut req = mask_request(slot);
    req["event"]["tool"]["result"] = json!({"nested":[{"stdout":slot}]});
    let d = support::Directory::new(json!({"pii":true,
        "pii_pattern":r#"MARKER|(?<="stdout":"MARKER )customer-123"#}));
    assert_eq!(run(req, &d)["effects"][0]["text"], WITHHELD);
}

#[test]
fn original_context_changing_a_retained_field_withholds_the_entire_result() {
    let slot = "MARKER";
    // Exercise both sides of the authorized slot and both usable and absent slot rewrites.
    for (result, context_rule) in [
        (
            json!({"slot":slot,"tail":"customer"}),
            r#"(?<="slot":"MARKER","tail":")customer"#,
        ),
        (
            json!({"head":"customer","slot":slot}),
            r#"customer(?=","slot":"MARKER")"#,
        ),
    ] {
        for pattern in [context_rule.to_owned(), format!("MARKER|{context_rule}")] {
            let d = support::Directory::new(json!({"pii":true,"pii_pattern":pattern}));
            let mut req = mask_request(slot);
            req["event"]["tool"]["result"] = result.clone();
            let response = run(req, &d);
            assert_eq!(response["error_code"], "unsupported_pii_result");
            assert!(response.get("effects").is_none());
        }
    }
}

#[test]
fn nested_risk_requires_a_complete_changed_full_json_redaction() {
    let result = json!({"stdout":SLOT,"exitCode":0});
    let raw = result.to_string();
    let mut req = mask_request(SLOT);
    req["event"]["tool"]["result"] = result;
    let report = json!({"ok":true,"verdict":"deny","summary":{
        "source":"tool_output","truncated":false,"bytes_scanned":raw.len(),"ruleset_id":"fixture-v2"}});
    let mut omitted = report.clone();
    omitted["redacted_text"] = json!("[REDACTED: output size limit]");
    omitted["summary"]["redacted_text_omitted"] = json!(true);
    let mut replies = vec![report.clone(), omitted];
    for text in ["", &raw] {
        let mut r = report.clone();
        r["redacted_text"] = json!(text);
        replies.push(r);
    }
    for reply in replies {
        let d = support::Directory::new(json!({"pii":true,
            "redactions_by_text":{&raw:{"stdout":reply}}}));
        assert_eq!(run(req.clone(), &d)["error_code"], "unsupported_pii_result");
    }
}

#[test]
fn nested_mask_rejects_a_changed_verdict_on_the_original_json() {
    let result = json!({"stdout":SLOT, "exitCode":0});
    let raw = result.to_string();
    let pass = json!({"ok":true,"verdict":"pass","summary":{
        "source":"tool_output","truncated":false,"bytes_scanned":raw.len(),"ruleset_id":"fixture-v2"}});
    let d =
        support::Directory::new(json!({"pii":true,"redactions_by_text":{&raw:{"stdout":pass}}}));
    let mut req = mask_request(SLOT);
    req["event"]["tool"]["result"] = result;
    let response = run(req, &d);
    assert_eq!(response["error_code"], "unsupported_pii_result");
    assert!(response.get("effects").is_none());
    let calls = scans(&d);
    assert_eq!(calls[0]["text"], raw);
    assert_eq!(calls.last().unwrap()["text"], raw);
    assert_eq!(calls.last().unwrap()["argv"][12], "--redact-output");
}

#[test]
fn decoded_only_slot_matches_allow_masking_after_the_full_json_passes() {
    let slot = "hello\n4111";
    let d = support::Directory::new(json!({"pii":true,"pii_pattern":r"(?<=\n)4111$"}));
    let mut req = mask_request(slot);
    req["event"]["tool"]["result"] = json!({"stdout":slot,"exitCode":0});
    let raw = req["event"]["tool"]["result"].to_string();
    assert_eq!(run(req, &d)["effects"][0]["text"], "hello\n[MASKED]");
    let original_scans: Vec<_> = scans(&d)
        .into_iter()
        .filter(|call| call["text"] == raw)
        .collect();
    assert_eq!(original_scans.len(), 2);
    assert_eq!(original_scans[0]["argv"].as_array().unwrap().len(), 12);
    assert_eq!(original_scans[1]["argv"][12], "--redact-output");
}

#[test]
fn mask_rechecks_decoded_strings_in_whole_result_projections() {
    for (nested, withheld) in [("public", false), ("private", true)] {
        let d = support::Directory::new(json!({"pii":true, "pii_pattern":"SENSITIVE|^private$"}));
        let result = json!({"content":SLOT,"nested":[nested]});
        let mut req = mask_request(&result.to_string());
        req["event"]["tool"]["result"] = result;
        let text = run(req, &d)["effects"][0]["text"].clone();
        if withheld {
            assert_eq!(text, WITHHELD);
        } else {
            assert_eq!(
                serde_json::from_str::<Value>(text.as_str().unwrap()).unwrap(),
                json!({"content":"contact [MASKED] for delivery", "nested":["public"]})
            );
        }
    }
}

#[test]
fn whole_projection_matches_the_redaction_of_each_changed_decoded_leaf() {
    let leaf = "记录🙂 \"item\"\\id\tMARKER\ncustomer-123";
    let original = json!({"items":[{"note":leaf}, "public"],"count":2});
    let mut req = mask_request(&original.to_string());
    req["event"]["tool"]["result"] = original.clone();
    for (pattern, withheld) in [
        ("MARKER", false),
        (r"MARKER|(?<=MARKER\n)customer-123", true),
    ] {
        let d = support::Directory::new(json!({"pii":true,"pii_pattern":pattern}));
        let response = run(req.clone(), &d);
        let text = response["effects"][0]["text"].as_str().unwrap();
        if withheld {
            assert_eq!(text, WITHHELD);
        } else {
            assert_eq!(text, original.to_string().replace("MARKER", "[MASKED]"));
        }
        let redacted_inputs: Vec<_> = scans(&d)
            .into_iter()
            .filter(|call| call["argv"][12] == "--redact-output")
            .map(|call| call["text"].clone())
            .collect();
        assert_eq!(redacted_inputs, [json!(original.to_string()), json!(leaf)]);
    }
    let d = support::Directory::new(json!({"pii":true,"pii_pattern":"MARKER",
        "redactions_by_text":{leaf:{"exit":1}}}));
    let response = run(req, &d);
    assert_eq!(response["error_code"], "cli_failed");
    assert!(response.get("effects").is_none());
}

#[test]
fn masked_whole_projection_with_changed_or_colliding_keys_is_withheld() {
    let d = support::Directory::new(json!({"pii":true, "pii_pattern":"SENSITIVE[12]"}));
    for result in [
        json!({"SENSITIVE1":"first","SENSITIVE2":"second"}),
        json!({"SENSITIVE1":"first","public":"second"}),
    ] {
        let mut req = mask_request(&result.to_string());
        req["event"]["tool"]["result"] = result;
        assert_eq!(run(req, &d)["effects"][0]["text"], WITHHELD);
    }
}

#[test]
fn partial_redactions_are_checked_as_decoded_text() {
    let slot = "SENSITIVE\ncustomer";
    let candidate = "[MASKED]\ncustomer";
    for result in [json!(slot), json!({"stdout":slot})] {
        let d = support::Directory::new(json!({"pii":true,
            "pii_pattern":"SENSITIVE|\ncustomer$",
            "redactions_by_text":{slot:{"stdout":{"ok":true,"verdict":"warn","redacted_text":candidate,
                "summary":{"source":"tool_output","truncated":false,"bytes_scanned":slot.len(),"ruleset_id":"fixture-v2"}}}}}));
        let mut req = mask_request(slot);
        req["event"]["tool"]["result"] = result;
        assert_eq!(
            run(req, &d)["effects"],
            json!([{"type":"replace_result","reason_code":"pii_risk","text":WITHHELD}])
        );
        assert!(scans(&d)
            .iter()
            .any(|c| c["text"] == candidate && c["argv"].as_array().unwrap().len() == 12));
    }
}

#[test]
fn malformed_redaction_omission_flags_are_protocol_errors() {
    for flag in [json!("false"), json!(1), Value::Null] {
        let d = support::Directory::new(json!({"pii":true,"redaction":{"stdout":{
            "ok":true,"verdict":"warn","redacted_text":"[MASKED]","summary":{
                "source":"tool_output","truncated":false,"bytes_scanned":SLOT.len(),
                "redacted_text_omitted":flag}}}}));
        let response = run(mask_request(SLOT), &d);
        assert_eq!(response["error_code"], "invalid_scan_result");
        assert!(response.get("effects").is_none());
    }
}

#[test]
fn unusable_redactions_withhold_the_entire_slot() {
    let report = json!({"ok":true, "verdict":"warn", "summary":{
        "source":"tool_output","truncated":false,"bytes_scanned":SLOT.len(),"ruleset_id":"fixture-v2"}});
    let mut omitted = report.clone();
    omitted["redacted_text"] = json!("[REDACTED: output size limit]");
    omitted["summary"]["redacted_text_omitted"] = json!(true);
    let mut pass = report.clone();
    pass["verdict"] = json!("pass");
    pass["redacted_text"] = json!("unusable");
    let mut replies = vec![report.clone(), omitted, pass];
    // The escaped text fits on its own, but the full AW response exceeds its limit.
    for text in [
        String::new(),
        SLOT.into(),
        "\n".repeat(aw_provider::MAX_MESSAGE_BYTES / 2 - 64),
    ] {
        let mut reply = report.clone();
        reply["redacted_text"] = json!(text);
        replies.push(reply);
    }
    for reply in replies {
        let d = support::Directory::new(json!({"pii":true,"redaction":{"stdout":reply}}));
        assert_eq!(
            run(mask_request(SLOT), &d)["effects"],
            json!([{"type":"replace_result","reason_code":"pii_risk","text":WITHHELD}])
        );
    }

    let d = support::Directory::new(json!({"pii":true,"pii_pattern":r#"(?<="value":")private"#}));
    let mut req = mask_request("private");
    req["event"]["tool"]["result"] = json!({"value":"private"});
    assert_eq!(run(req, &d)["effects"][0]["text"], WITHHELD);
}

#[test]
fn redaction_errors_follow_the_invocation_error_path() {
    for (redaction, error) in [
        (json!({"exit":1}), "cli_failed"),
        (json!({"raw":"broken"}), "invalid_scan_result"),
        (
            json!({"stdout":{"ok":true,"verdict":"deny","redacted_text":"untrusted",
            "summary":{"source":"tool_output","bytes_scanned":SLOT.len(),"truncated":false,
                "coverage":{"status":"partial"}}}}),
            "incomplete_pii_scan",
        ),
        (json!({"sleep":10}), "deadline_exceeded"),
    ] {
        let d = support::Directory::new(json!({"pii":true,"redaction":redaction}));
        let mut req = mask_request(SLOT);
        req["budget_ms"] = json!(1500);
        let response = run(req, &d);
        assert_eq!(response["error_code"], error);
        assert!(response.get("effects").is_none());
        assert_eq!(scans(&d).last().unwrap()["argv"][12], "--redact-output");
        d.assert_stopped();
    }
}
