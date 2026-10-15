use aw_provider::{Error, Protocol, Reply, MAX_DEPTH, MAX_MESSAGE_BYTES, VERSION};
use serde_json::{json, Value};

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn request(method: &str) -> Value {
    json!({"api_version": VERSION, "method": method, "request_id": "request-1"})
}

fn response() -> Value {
    json!({"api_version": VERSION, "request_id": "request-1", "status": "ok"})
}

fn invoke(event: &str, effects: Value) -> Value {
    let mut value = request("invoke");
    value["operation"] = json!("check");
    value["config_revision"] = json!("a".repeat(64));
    value["budget_ms"] = json!(250);
    value["allowed_effects"] = effects;
    value["config"] = json!({"阈值": 0.25});
    value["event"] = json!({
        "name": event,
        "agent": {"adapter": "qoder", "binding_id": "work", "instance_id": null},
        "session_id": null,
        "tool": {"name": "mcp__custom__查询", "native_name": "mcp__custom__查询",
                 "call_id": null, "input": {"价格": 1.25}, "result": null},
        "native": {"failed": true}
    });
    value
}

#[test]
fn methods_keep_description_private_config_and_candidate_effects_distinct() {
    let protocol = Protocol::new().unwrap();
    let describe = protocol
        .parse_request(&bytes(&request("describe")))
        .unwrap();
    let mut reply = response();
    reply["operations"] = json!([{
        "name": "check", "events": ["tool.before", "tool.after"],
        "effects": ["observe", "block"]
    }]);
    assert!(matches!(
        protocol.check_response(&describe, &bytes(&reply)),
        Ok(Reply::Description(_))
    ));
    let mut validation = request("validate_config");
    validation["config"] = json!({"阈值": 1.25, "<<": "ordinary JSON key"});
    let validation = protocol.parse_request(&bytes(&validation)).unwrap();
    let Reply::Configuration(checked) = protocol
        .check_response(&validation, &bytes(&response()))
        .unwrap()
    else {
        panic!("expected configuration")
    };
    assert_eq!(checked.as_value(), &validation.as_value()["config"]);
    let invoke = protocol
        .bind_invocation(invoke("tool.before", json!(["observe", "block"])))
        .unwrap();
    let mut reply = response();
    reply["input_digest"] = invoke.as_value()["input_digest"].clone();
    reply["effects"] = json!([{"type": "block", "reason_code": "policy_match"}]);
    let Reply::Invocation(outcome) = protocol.check_response(&invoke, &bytes(&reply)).unwrap()
    else {
        panic!("expected candidate effects")
    };
    assert!(outcome.requests_block());
    reply["effects"] = json!([]);
    let Reply::Invocation(outcome) = protocol.check_response(&invoke, &bytes(&reply)).unwrap()
    else {
        panic!("expected neutral candidate")
    };
    assert!(!outcome.requests_block());
}

#[test]
fn response_identity_method_and_status_cannot_be_mixed() {
    let protocol = Protocol::new().unwrap();
    let describe = protocol
        .parse_request(&bytes(&request("describe")))
        .unwrap();
    for change in [
        json!({"request_id": "other"}),
        json!({"api_version": "aw-provider/v2"}),
        json!({"status": "ok", "error_code": "bad"}),
        json!({"method": "describe"}),
        json!({"receipt": {"disposition": "produced"}}),
    ] {
        let mut reply = response();
        reply["operations"] =
            json!([{"name":"a", "events":["tool.before"], "effects":["observe"]}]);
        reply
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        assert!(protocol.check_response(&describe, &bytes(&reply)).is_err());
    }
    assert!(protocol
        .check_response(&describe, &bytes(&response()))
        .is_err());
    let mut validation = request("validate_config");
    validation["config"] = json!({});
    let validation = protocol.parse_request(&bytes(&validation)).unwrap();
    let mut wrong = response();
    wrong["effects"] = json!([]);
    wrong["input_digest"] = json!(format!("sha256:{}", "a".repeat(64)));
    assert!(protocol
        .check_response(&validation, &bytes(&wrong))
        .is_err());
}

#[test]
fn provider_errors_are_not_blocks_and_do_not_leak_diagnostics() {
    let protocol = Protocol::new().unwrap();
    let describe = protocol
        .parse_request(&bytes(&request("describe")))
        .unwrap();
    let reply = json!({"api_version":VERSION,"request_id":"request-1",
                       "status":"error","error_code":"private_rule_failure"});
    let error = match protocol.check_response(&describe, &bytes(&reply)) {
        Err(error) => error,
        Ok(_) => panic!("error must not succeed"),
    };
    assert!(!error.to_string().contains("private_rule_failure"));
    assert!(matches!(error, Error::ProviderFailure { code } if code == "private_rule_failure"));
    for code in ["", "secret\n", "secret password", "../secret/path"] {
        let mut invalid = reply.clone();
        invalid["error_code"] = json!(code);
        assert!(matches!(
            protocol.check_response(&describe, &bytes(&invalid)),
            Err(Error::Invalid(_))
        ));
    }
}

#[test]
fn descriptions_reject_ambiguous_or_unknown_capabilities() {
    let protocol = Protocol::new().unwrap();
    let describe = protocol
        .parse_request(&bytes(&request("describe")))
        .unwrap();
    let op = json!({"name":"check","events":["tool.before"],"effects":["observe"]});
    for operations in [
        json!([]),
        json!([op.clone(), op.clone()]),
        json!(vec![op.clone(); 65]),
    ] {
        let mut reply = response();
        reply["operations"] = operations;
        assert!(protocol.check_response(&describe, &bytes(&reply)).is_err());
    }
    for change in [
        json!({"events":["invented.event"]}),
        json!({"effects":["allow"]}),
        json!({"events":["tool.before","tool.before"]}),
        json!({"effects":[]}),
        json!({"name":"bad\n"}),
        json!({"authority":"enforce"}),
    ] {
        let mut changed = op.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        let mut reply = response();
        reply["operations"] = json!([changed]);
        assert!(protocol.check_response(&describe, &bytes(&reply)).is_err());
    }
}

#[test]
fn invocation_binds_config_event_budget_and_identity_without_canonical_coercion() {
    let protocol = Protocol::new().unwrap();
    let request = protocol
        .bind_invocation(invoke("tool.before", json!(["observe"])))
        .unwrap();
    assert_eq!(request.as_value()["config"]["阈值"], 0.25);
    assert_eq!(request.as_value()["event"]["tool"]["input"]["价格"], 1.25);
    protocol.parse_request(&bytes(request.as_value())).unwrap();
    for field in [
        "config",
        "config_revision",
        "event",
        "budget_ms",
        "request_id",
        "input_digest",
    ] {
        let mut changed = request.as_value().clone();
        changed[field] = match field {
            "config" => json!({}),
            "event" => invoke("tool.after", json!(["observe"]))["event"].clone(),
            "budget_ms" => json!(200),
            "request_id" => json!("other"),
            "config_revision" => json!("b".repeat(64)),
            _ => json!(format!("sha256:{}", "0".repeat(64))),
        };
        assert!(protocol.parse_request(&bytes(&changed)).is_err(), "{field}");
    }
    assert!(protocol
        .bind_invocation(request.as_value().clone())
        .is_err());
}

#[test]
fn hook_binding_preserves_event_limits_without_relaxing_provider_wire_limits() {
    let protocol = Protocol::new().unwrap();
    let mut value = invoke("tool.before", json!(["block"]));
    value["operation"] = json!("invoke");
    value["config"] = json!({});
    value["event"]["tool"]["input"] = json!("");
    let overhead = bytes(&value["event"]).len();
    value["event"]["tool"]["input"] = json!("I".repeat(MAX_MESSAGE_BYTES - overhead));
    assert_eq!(bytes(&value["event"]).len(), MAX_MESSAGE_BYTES);
    assert!(protocol.bind_invocation(value.clone()).is_err());
    let hook = protocol.bind_hook_invocation(value.clone()).unwrap();
    assert!(protocol.parse_request(&bytes(hook.as_value())).is_err());
    let mut reply = response();
    reply["input_digest"] = hook.as_value()["input_digest"].clone();
    reply["effects"] = json!([{"type":"block"}]);
    assert!(matches!(
        protocol.check_response(&hook, &bytes(&reply)),
        Ok(Reply::Invocation(_))
    ));
    value["event"]["tool"]["input"] = json!("I".repeat(MAX_MESSAGE_BYTES - overhead + 1));
    assert!(protocol.bind_hook_invocation(value.clone()).is_err());
    let mut nested = json!("leaf");
    for _ in 0..MAX_DEPTH - 2 {
        nested = json!([nested]);
    }
    value["event"]["tool"]["input"] = nested.clone();
    assert!(protocol.bind_invocation(value.clone()).is_err());
    let hook = protocol.bind_hook_invocation(value.clone()).unwrap();
    assert!(protocol.parse_request(&bytes(hook.as_value())).is_err());
    value["event"]["tool"]["input"] = json!([nested]);
    assert!(protocol.bind_hook_invocation(value).is_err());
}

#[test]
fn hook_binding_keeps_request_shape_and_capability_validation() {
    let protocol = Protocol::new().unwrap();
    let mut value = invoke("tool.before", json!(["block"]));
    value["operation"] = json!("invoke");
    value["config"] = json!({});
    let hook = protocol.bind_hook_invocation(value.clone()).unwrap();
    let provider = protocol.bind_invocation(value.clone()).unwrap();
    assert_eq!(
        hook.as_value()["input_digest"],
        provider.as_value()["input_digest"]
    );
    for (pointer, replacement) in [
        ("/method", json!("describe")),
        ("/operation", json!("check")),
        ("/config", json!({"ignored":true})),
        ("/request_id", json!("R".repeat(129))),
        ("/config_revision", json!("invalid")),
        ("/budget_ms", json!(0)),
        ("/allowed_effects", json!(["ask"])),
        ("/event/tool/result", json!({"unexpected":true})),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert!(protocol.bind_hook_invocation(invalid).is_err(), "{pointer}");
    }
    let mut invalid = value.clone();
    invalid["input_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert!(protocol.bind_hook_invocation(invalid).is_err());
    value["unknown"] = json!(true);
    assert!(protocol.bind_hook_invocation(value).is_err());
}

#[test]
fn after_preserves_null_structured_multimodal_and_failed_results() {
    let protocol = Protocol::new().unwrap();
    for result in [
        Value::Null,
        json!("serialized native result"),
        json!([{"type":"image","data":"opaque"}]),
        json!({"error":"denied","amount":1.25}),
    ] {
        let mut value = invoke("tool.after", json!(["observe"]));
        value["event"]["tool"]["result"] = result.clone();
        let request = protocol.bind_invocation(value).unwrap();
        assert_eq!(request.as_value()["event"]["tool"]["result"], result);
        assert_eq!(request.as_value()["event"]["native"]["failed"], true);
        assert!(request.as_value()["event"].get("success").is_none());
    }
    let mut before = invoke("tool.before", json!(["observe"]));
    before["event"]["tool"]["result"] = json!("not executed yet");
    assert!(protocol.bind_invocation(before).is_err());
}

#[test]
fn responses_cannot_escalate_effects_or_change_request_binding() {
    let protocol = Protocol::new().unwrap();
    let request = protocol
        .bind_invocation(invoke("tool.before", json!(["observe"])))
        .unwrap();
    let mut reply = response();
    reply["input_digest"] = request.as_value()["input_digest"].clone();
    for effects in [
        json!([{"type":"block"}]),
        json!([{"type":"ask"}]),
        json!([{"type":"replace_input"}]),
        json!([{"type":"observe","reason_code":"secret\n"}]),
        json!([{"type":"observe","text":"secret"}]),
        json!(vec![json!({"type":"observe"}); 65]),
    ] {
        reply["effects"] = effects;
        assert!(protocol.check_response(&request, &bytes(&reply)).is_err());
    }
    reply["effects"] = json!([]);
    reply["input_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert!(protocol.check_response(&request, &bytes(&reply)).is_err());
    for (event, effect) in [
        ("tool.before", "ask"),
        ("tool.before", "replace_input"),
        ("tool.after", "block"),
        ("tool.before", "replace_result"),
    ] {
        assert!(protocol
            .bind_invocation(invoke(event, json!([effect])))
            .is_err());
    }
}

#[test]
fn parser_rejects_duplicate_keys_trailing_documents_and_resource_overflow() {
    let protocol = Protocol::new().unwrap();
    let prefix = format!(
        r#"{{"api_version":"{VERSION}","request_id":"r","method":"validate_config","config":"#
    );
    for config in [
        r#"{"key":1,"key":2}"#,
        r#"{"a":{"key":1,"key":2}}"#,
        r#"{"a":NaN}"#,
    ] {
        assert!(protocol
            .parse_request(format!("{prefix}{config}}}").as_bytes())
            .is_err());
    }
    for message in [
        bytes(&request("describe")),
        vec![b' '; MAX_MESSAGE_BYTES + 1],
        b"---\nmethod: describe".to_vec(),
        vec![0xff],
    ] {
        let mut invalid = message;
        invalid.extend_from_slice(b"{}");
        assert!(protocol.parse_request(&invalid).is_err());
    }
    let mut nested = Value::Null;
    for _ in 0..MAX_DEPTH + 1 {
        nested = json!([nested]);
    }
    let mut validation = request("validate_config");
    validation["config"] = json!({"nested":nested});
    assert!(protocol.parse_request(&bytes(&validation)).is_err());
    let describe = protocol
        .parse_request(&bytes(&request("describe")))
        .unwrap();
    let duplicate = format!(
        r#"{{"api_version":"{VERSION}","request_id":"r","request_id":"request-1","status":"error","error_code":"no"}}"#
    );
    assert!(protocol
        .check_response(&describe, duplicate.as_bytes())
        .is_err());
}

#[test]
fn protocol_event_vocabulary_matches_configuration() {
    let config: Value = serde_json::from_str(aw_config::SCHEMA).unwrap();
    let response: Value = serde_json::from_str(aw_provider::RESPONSE_SCHEMA).unwrap();
    let events = config["properties"]["spec"]["properties"]["events"]["properties"]
        .as_object()
        .unwrap();
    let declared = response["oneOf"][0]["properties"]["operations"]["items"]["properties"]
        ["events"]["items"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(events.len(), 16);
    assert_eq!(declared.len(), events.len());
    assert!(events.keys().all(|event| declared.contains(&json!(event))));
}

#[test]
fn integer_tokens_cannot_silently_become_rounded_floats() {
    let protocol = Protocol::new().unwrap();
    let prefix = format!(
        r#"{{"api_version":"{VERSION}","request_id":"r","method":"validate_config","config":{{"id":"#
    );
    for number in [
        "18446744073709551616",
        "18446744073709551617",
        "-9223372036854775809",
    ] {
        let input = format!("{prefix}{number}}}}}");
        assert!(
            protocol.parse_request(input.as_bytes()).is_err(),
            "{number}"
        );
    }
    for number in [
        "18446744073709551615",
        "-9223372036854775808",
        "0.25",
        "1e3",
    ] {
        let input = format!("{prefix}{number}}}}}");
        assert!(protocol.parse_request(input.as_bytes()).is_ok(), "{number}");
    }
    let mut value = request("validate_config");
    value["config"] = json!({"id": "18446744073709551617", "quoted": "\"-9223372036854775809"});
    assert!(protocol.parse_request(&bytes(&value)).is_ok());
}

#[test]
fn finite_float_parameters_round_trip_through_invocation_binding() {
    let protocol = Protocol::new().unwrap();
    for number in [
        0.29,
        1.0000000000000002,
        -261.7752280555518,
        f64::MIN_POSITIVE,
        f64::MAX,
    ] {
        let mut value = invoke("tool.before", json!(["observe"]));
        value["event"]["tool"]["input"] = json!({"number": number});
        let request = protocol.bind_invocation(value).unwrap();
        assert_eq!(
            request.as_value()["event"]["tool"]["input"]["number"],
            json!(number)
        );
        protocol.parse_request(&bytes(request.as_value())).unwrap();
    }
}

#[test]
fn block_text_is_a_bounded_reason_and_never_an_input_replacement() {
    let protocol = Protocol::new().unwrap();
    let request = protocol
        .bind_invocation(invoke("tool.before", json!(["block"])))
        .unwrap();
    let mut reply = response();
    reply["input_digest"] = request.as_value()["input_digest"].clone();
    reply["effects"] = json!([{"type":"block","text":"Retry with new arguments"}]);
    assert!(protocol.check_response(&request, &bytes(&reply)).is_ok());
    for text in [
        "".to_owned(),
        "x".repeat(aw_provider::MAX_MESSAGE_BYTES + 1),
    ] {
        reply["effects"][0]["text"] = json!(text);
        assert!(protocol.check_response(&request, &bytes(&reply)).is_err());
    }
    reply["effects"] = json!([{"type":"block","text":"retry","arguments":{"command":"execute"}}]);
    assert!(protocol.check_response(&request, &bytes(&reply)).is_err());
}

#[test]
fn replacement_requires_one_text_payload_and_a_projected_after_result() {
    let protocol = Protocol::new().unwrap();
    let mut value = invoke("tool.after", json!(["replace_result"]));
    value["event"]["tool"]["result_projection"] =
        json!({"text":"original","status":"success","content_origin":"api_response"});
    let request = protocol.bind_invocation(value.clone()).unwrap();
    let mut reply = response();
    reply["input_digest"] = request.as_value()["input_digest"].clone();
    reply["effects"] = json!([{"type":"replace_result","text":"compact"}]);
    assert!(matches!(
        protocol.check_response(&request, &bytes(&reply)),
        Ok(Reply::Invocation(_))
    ));
    for effects in [
        json!([{"type":"replace_result"}]),
        json!([{"type":"replace_result","text":""}]),
        json!([{"type":"replace_result","text":"x".repeat(aw_provider::MAX_MESSAGE_BYTES + 1)}]),
        json!([{"type":"observe","text":"hidden"}]),
        json!([{"type":"replace_result","text":"a"},{"type":"replace_result","text":"b"}]),
    ] {
        reply["effects"] = effects;
        assert!(protocol.check_response(&request, &bytes(&reply)).is_err());
    }
    value["event"]["tool"]
        .as_object_mut()
        .unwrap()
        .remove("result_projection");
    let request = protocol.bind_invocation(value).unwrap();
    reply["input_digest"] = request.as_value()["input_digest"].clone();
    reply["effects"] = json!([{"type":"replace_result","text":"compact"}]);
    assert!(protocol.check_response(&request, &bytes(&reply)).is_err());
    assert!(protocol
        .bind_invocation(invoke("tool.before", json!(["replace_result"])))
        .is_err());
}
