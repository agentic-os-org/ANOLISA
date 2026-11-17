use super::*;

#[test]
fn codex_replaces_plain_and_single_text_mcp_results() {
    for response in [
        json!("original"),
        json!({"content":[{"type":"text","text":"secret"}],
            "structuredContent":{"private":"secret"}}),
    ] {
        let native = json!({"hook_event_name":"PostToolUse","tool_name":"mcp__fixture__echo",
            "tool_input":{},"tool_response":response});
        assert!(project("codex", &native).is_some());
        let output = reply("codex", &native, "safe").unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            json!({"continue":false,"reason":"safe"})
        );
    }
    let media = json!({"hook_event_name":"PostToolUse","tool_name":"mcp__fixture__image",
        "tool_input":{},"tool_response":{"content":[{"type":"image","data":"secret"}]}});
    assert!(project("codex", &media).is_none());
    assert!(reply("codex", &media, "safe").is_err());
    let partial = json!({"hook_event_name":"PostToolUse","tool_name":"Bash",
        "tool_input":{},"tool_response":{"stdout":"safe","stderr":"secret"}});
    assert!(project("codex", &partial).is_none());
    assert!(reply("codex", &partial, "safe").is_err());
}

#[test]
fn command_evidence_precedes_tool_names_and_names_remain_the_fallback() {
    for (name, input, origin) in [
        (
            "mcp__local__shell",
            json!({"command":"git status"}),
            "command_output",
        ),
        (
            "read_file",
            json!({"command":"git status"}),
            "command_output",
        ),
        ("read_file", json!({"path":"file.txt"}), "file_content"),
        ("mcp__local__shell", json!({}), "api_response"),
        ("Bash", json!({}), "command_output"),
        ("read_file", json!({"command":42}), "file_content"),
    ] {
        let native = json!({"tool_name":name,"tool_input":input,"tool_response":"output"});
        let projected = project("qoder", &native).unwrap();
        assert_eq!(projected["content_origin"], origin, "{name}: {input}");
        assert_eq!(
            projected.get("command").and_then(Value::as_str),
            input["command"].as_str()
        );
        let mut failed = native.clone();
        failed["tool_response"] = json!({"stdout":"failure","isError":true});
        assert_eq!(project("qoder", &failed).unwrap()["status"], "error");
        failed["tool_response"] = json!({"content":[{"type":"image","data":"opaque"}]});
        assert!(project("qoder", &failed).is_none());
    }
}

#[test]
fn qwenpaw_origin_uses_the_same_decoded_command_as_the_projection() {
    for (input, origin) in [
        (r#"{"command":"git status"}"#, "command_output"),
        (r#"{"path":"file.txt"}"#, "file_content"),
        ("malformed JSON", "file_content"),
    ] {
        let native = json!({"tool_call":{"name":"read_file","input":input},
            "tool_response":{"state":"success","content":[{"type":"text","text":"output"}]}});
        let projected = project("qwenpaw", &native).unwrap();
        assert_eq!(projected["content_origin"], origin);
        assert_eq!(
            projected.get("command").and_then(Value::as_str),
            (origin == "command_output").then_some("git status")
        );
    }
    let missing_input = json!({"tool_call":{"name":"read_file"},
        "tool_response":{"state":"success","content":[{"type":"text","text":"output"}]}});
    assert!(project("qwenpaw", &missing_input).is_none());
}

#[test]
fn claude_replaces_only_completed_bash_stdout_and_preserves_the_envelope() {
    let native = json!({"hook_event_name":"PostToolUse", "tool_name":"Bash", "tool_input":{"command":"git status"}, "tool_response":{"stdout":"original long output", "stderr":"warning", "interrupted":false, "isImage":false, "content":[{"type":"text","text":"metadata"}], "metadata":{"kept":true}}});
    let projection = project("claude-sdk", &native).unwrap();
    assert_eq!(projection["command"], "git status");
    assert_eq!(projection["status"], "success");
    assert_eq!(projection["text"], "original long output");
    let response: Value =
        serde_json::from_slice(&reply("claude-sdk", &native, "compact").unwrap().stdout).unwrap();
    let mut expected = native["tool_response"].clone();
    expected["stdout"] = json!("compact");
    assert_eq!(
        response,
        json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":expected}})
    );
    for (key, value) in [
        ("hook_event_name", json!("PostToolUseFailure")),
        ("tool_name", json!("WebSearch")),
        (
            "tool_response",
            json!({"stdout":"image", "isImage":true, "interrupted":false}),
        ),
        (
            "tool_response",
            json!({"stdout":"partial", "isImage":false, "interrupted":true}),
        ),
    ] {
        let mut unsupported = native.clone();
        unsupported[key] = value;
        assert!(project("claude-sdk", &unsupported).is_none());
        assert!(reply("claude-sdk", &unsupported, "compact").is_err());
    }
}

#[test]
fn claude_mcp_projection_replaces_the_model_visible_string() {
    // structuredContent present: the hook sees the serialized string.
    let structured = json!({
        "hook_event_name":"PostToolUse",
        "tool_name":"mcp__inspection__inspect_runtime",
        "tool_input":{"query":"tenant"},
        "tool_response":"{\"tenant\":\"fixture-tenant\",\"rows\":[]}"
    });
    let projection = project("claude-sdk", &structured).unwrap();
    assert_eq!(
        projection["text"],
        "{\"tenant\":\"fixture-tenant\",\"rows\":[]}"
    );
    assert_eq!(projection["status"], "success");
    assert_eq!(projection["content_origin"], "api_response");
    let response: Value =
        serde_json::from_slice(&reply("claude-sdk", &structured, "compact").unwrap().stdout)
            .unwrap();
    assert_eq!(
        response,
        json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"compact"}})
    );

    // plain text only: the hook sees the content blocks.
    let plain = json!({
        "hook_event_name":"PostToolUse",
        "tool_name":"mcp__local__touch",
        "tool_input":{"value":"tool_native_1"},
        "tool_response":[{"type":"text","text":"original long output"}]
    });
    let projection = project("claude-sdk", &plain).unwrap();
    assert_eq!(projection["text"], "original long output");
    let response: Value =
        serde_json::from_slice(&reply("claude-sdk", &plain, "compact").unwrap().stdout).unwrap();
    assert_eq!(
        response,
        json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"compact"}})
    );

    // multi-block, non-text, object and missing projections stay untouched.
    for tool_response in [
        json!([{"type":"text","text":"a"},{"type":"text","text":"b"}]),
        json!([{"type":"image","data":"..."}]),
        json!({"stdout":"object shape"}),
        Value::Null,
    ] {
        let mut unsupported = plain.clone();
        unsupported["tool_response"] = tool_response;
        assert!(project("claude-sdk", &unsupported).is_none());
        assert!(reply("claude-sdk", &unsupported, "compact").is_err());
    }

    // Measured CLI 2.1.161 shape for HTTP MCP isError results: the
    // failure event carries no tool_response, only error and is_interrupt,
    // so failed results have no replaceable slot at all.
    let failure = json!({
        "hook_event_name":"PostToolUseFailure",
        "tool_name":"mcp__local__touch",
        "tool_input":{"value":"tool_native_1"},
        "tool_use_id":"tool_native_1",
        "error":"fixture tool failure",
        "is_interrupt":false
    });
    assert!(project("claude-sdk", &failure).is_none());
    assert!(reply("claude-sdk", &failure, "compact").is_err());
}

#[test]
fn agentscope_projection_uses_effective_command_instead_of_raw_json() {
    let native = json!({"tool_call":{"name":"read_file","input":"{\"command\":\"raw"},
        "effective_input":{"command":"echo repaired"},
        "tool_response":{"state":"success","content":[{"type":"text","text":"output"}]}});
    assert_eq!(
        project("agentscope", &native).unwrap()["content_origin"],
        "command_output"
    );
    assert_eq!(
        project("agentscope", &native).unwrap()["command"],
        "echo repaired"
    );
}

#[test]
fn empty_or_unbounded_replacements_are_rejected_for_every_adapter() {
    for adapter in ["qoder", "openclaw", "qwenpaw", "hermes", "pi", "agentscope"] {
        assert!(reply(adapter, &json!({}), "").is_err());
        assert!(reply(
            adapter,
            &json!({}),
            &"x".repeat(aw_provider::MAX_MESSAGE_BYTES + 1)
        )
        .is_err());
    }
}

#[test]
fn deepseek_preserves_canonical_data_and_excludes_nested_or_multiblock_results() {
    let mut native = json!({"nested":false, "tool_call":{"name":"bash","input":{"command":"ls"}},
        "tool_response":{"content":[{"type":"text","text":"original"}],"isError":false,
            "value":{"canonical":true},"meta":{"preserved":true}}});
    assert_eq!(
        project("deepseek-harness", &native).unwrap()["status"],
        "success"
    );
    let replaced: Value = serde_json::from_slice(
        &reply("deepseek-harness", &native, "compact")
            .unwrap()
            .stdout,
    )
    .unwrap();
    let mut expected = native["tool_response"].clone();
    expected["content"][0]["text"] = json!("compact");
    assert_eq!(replaced["aw_replacement"], expected);
    native["tool_response"]["isError"] = json!(true);
    assert!(project("deepseek-harness", &native).is_none());
    native["nested"] = json!(true);
    assert!(project("deepseek-harness", &native).is_none());
    assert!(reply("deepseek-harness", &native, "compact").is_err());
    native["nested"] = json!(false);
    native["tool_response"]["content"] =
        json!([{"type":"text","text":"one"},{"type":"text","text":"two"}]);
    assert!(project("deepseek-harness", &native).is_none());
    assert!(reply("deepseek-harness", &native, "compact").is_err());
}

#[test]
fn pi_projects_object_arguments_and_effective_sdk_error_status() {
    let mut native = json!({"tool_call":{"name":"bash","input":{"command":"git status"}},
        "tool_response":{"content":[{"type":"text","text":"original"}],"isError":false,"details":{"preserved":true}}});
    let projected = project("pi", &native).unwrap();
    assert_eq!(
        projected,
        json!({"text":"original","status":"success","content_origin":"command_output","command":"git status"})
    );
    native["tool_response"]["isError"] = json!(true);
    assert_eq!(project("pi", &native).unwrap()["status"], "error");
    for content in [
        json!([{"type":"image","data":"opaque"}]),
        json!([{"type":"text","text":"a"},{"type":"text","text":"b"}]),
    ] {
        native["tool_response"]["content"] = content;
        assert!(project("pi", &native).is_none());
        assert!(reply("pi", &native, "compact").is_err());
    }
}

#[test]
fn qoder_structured_result_has_an_explicit_string_envelope() {
    let native = json!({"tool_response":{"content":[{"type":"text","text":"original"}],"metadata":{"kept":true}}});
    let encoded: Value =
        serde_json::from_slice(&reply("qoder", &native, "compact").unwrap().stdout).unwrap();
    assert_eq!(
        encoded["hookSpecificOutput"]["updatedToolOutput"],
        r#"{"content":[{"text":"compact","type":"text"}],"metadata":{"kept":true}}"#
    );
}

#[test]
fn qoder_sdk_read_projects_and_replaces_only_its_native_text_slot() {
    let native = json!({"hook_event_name":"PostToolUse", "tool_name":"Read",
        "tool_input":{"file_path":"file.txt"},
        "tool_response":{"type":"text", "file":{"filePath":"file.txt",
            "content":"original", "numLines":1, "startLine":1, "totalLines":1},
            "metadata":{"kept":true}}});
    assert_eq!(
        project("qoder-sdk", &native),
        Some(json!({"text":"original", "status":"success", "content_origin":"file_content"}))
    );
    let output: Value =
        serde_json::from_slice(&reply("qoder-sdk", &native, "safe").unwrap().stdout).unwrap();
    let adopted: Value = serde_json::from_str(
        output["hookSpecificOutput"]["updatedToolOutput"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let mut expected = native["tool_response"].clone();
    expected["file"]["content"] = json!("safe");
    assert_eq!(adopted, expected);

    for (key, value) in [
        ("tool_name", json!("Bash")),
        ("hook_event_name", json!("PostToolUseFailure")),
        (
            "tool_response",
            json!({"type":"image", "file":{"content":"pixels"}}),
        ),
        (
            "tool_response",
            json!({"type":"text", "file":{"content":42}}),
        ),
        (
            "tool_response",
            json!({"type":"text", "file":{"content":"original"},
                "content":[{"type":"image","data":"opaque"}]}),
        ),
        (
            "tool_response",
            json!({"type":"text", "file":{"content":"original"},
                "content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}),
        ),
    ] {
        let mut unsupported = native.clone();
        unsupported[key] = value;
        assert!(project("qoder-sdk", &unsupported).is_none());
        assert!(reply("qoder-sdk", &unsupported, "safe").is_err());
    }
    for adapter in [
        "qoder",
        "claude-sdk",
        "openclaw",
        "qwenpaw",
        "pi",
        "agentscope",
        "hermes",
    ] {
        assert!(project(adapter, &native).is_none());
        assert!(reply(adapter, &native, "safe").is_err());
    }
}

#[test]
fn openclaw_details_failures_are_not_projected_as_success() {
    for details in [
        json!({"exitCode":127}),
        json!({"status":"completed","exitCode":127}),
        json!({"ok":true,"exitCode":127}),
        json!({"status":"failed"}),
        json!({"timedOut":true}),
        json!({"ok":false}),
        json!({"success":false}),
        json!({"status":" TIMED_OUT "}),
        json!({"status":"denied"}),
        json!({"status":"cancelled"}),
        json!({"status":"completed","error":{"message":"failure"}}),
        json!({"status":"failed","ok":true,"timedOut":true}),
    ] {
        let native = json!({"event":{"result":{"content":[{"type":"text","text":"failure"}],"isError":false,"details":details}}});
        let projection = project("openclaw", &native).unwrap();
        assert_eq!(projection["status"], "error", "{details}");
        assert_eq!(projection["text"], "failure");
    }
    for details in [
        json!(null),
        json!({}),
        json!({"exitCode":0,"timedOut":false,"ok":true}),
        json!({"status":"failed","ok":true}),
        json!({"status":"error","success":true}),
        json!({"error":false}),
        json!({"error":0}),
        json!({"error":""}),
    ] {
        let native = json!({"event":{"result":{"content":[{"type":"text","text":"original"}],"details":details}}});
        assert_eq!(
            project("openclaw", &native).unwrap()["status"],
            "success",
            "{details}"
        );
    }
}

#[test]
fn text_replacement_preserves_native_metadata_and_does_not_add_a_copy() {
    for adapter in ["openclaw", "qwenpaw", "pi", "agentscope"] {
        let original = json!({"content":[{"type":"text","text":"original","annotations":{"kept":true}}],"metadata":{"kept":42},"state":"success"});
        let native = if adapter == "openclaw" {
            json!({"event":{"result":original}})
        } else {
            json!({"tool_response":original})
        };
        let reply: Value =
            serde_json::from_slice(&reply(adapter, &native, "compressed").unwrap().stdout).unwrap();
        let mut expected = original;
        expected["content"][0]["text"] = json!("compressed");
        assert_eq!(reply["aw_replacement"], expected);
    }
}

#[test]
fn media_and_multiple_blocks_have_no_projection() {
    for content in [
        json!([{"type":"image","data":"opaque"}]),
        json!([{"type":"text","text":"a"},{"type":"text","text":"b"}]),
    ] {
        for adapter in ["openclaw", "qwenpaw", "agentscope"] {
            let native = if adapter == "openclaw" {
                json!({"event":{"result":{"content":content}}})
            } else {
                json!({"tool_response":{"content":content,"state":"success"}})
            };
            assert!(project(adapter, &native).is_none());
        }
    }
}

#[test]
fn hermes_preserves_terminal_envelope_and_file_origin() {
    let native = json!({"tool_name":"terminal", "tool_input":{"command":"cat file.json"}, "extra":{"status":"ok", "result":"{\"output\":\"original\",\"exit_code\":0}"}});
    assert_eq!(
        project("hermes", &native).unwrap()["command"],
        "cat file.json"
    );
    let response: Value =
        serde_json::from_slice(&reply("hermes", &native, "compact").unwrap().stdout).unwrap();
    assert_eq!(
        response["aw_replacement"],
        r#"{"exit_code":0,"output":"compact"}"#
    );
    let result: Value = serde_json::from_str(response["aw_replacement"].as_str().unwrap()).unwrap();
    assert_eq!(result, json!({"output":"compact","exit_code":0}));
}
