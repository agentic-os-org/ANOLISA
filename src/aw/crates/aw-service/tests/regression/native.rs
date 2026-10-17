//! Native callback fixtures for every registered agent in the regression matrix.

use serde_json::{json, Value};
use std::path::Path;

pub fn agents() -> Vec<String> {
    let matrix: Value =
        serde_json::from_str(include_str!("../../../../tests/regression-matrix.json")).unwrap();
    matrix["agents"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

pub fn callback(adapter: &str, event: &str, call: &str, input: Value, cwd: &Path) -> Value {
    let before = event == "tool.before";
    let mut native = match adapter {
        "qoder" | "claude-sdk" => {
            let response = if adapter == "claude-sdk" {
                json!({"stdout":"original", "stderr":"", "interrupted":false, "isImage":false})
            } else {
                json!("original")
            };
            json!({"hook_event_name":if before {"PreToolUse"} else {"PostToolUse"},
                "cwd":cwd,"session_id":"session","tool_use_id":call,"tool_name":"Bash",
                "tool_input":input,"tool_response":response})
        }
        "openclaw" => json!({"hook":if before {"before_tool_call"} else {"after_tool_call"},
            "event":{"toolName":if before {"exec"} else {"Custom/Tool"},"params":input,
                "toolCallId":call,"runId":"run","result":{"content":[{"type":"text","text":"original"}]}},
            "context":{"sessionId":"session","runId":"run","toolCallId":call}}),
        "agentscope" | "qwenpaw" => json!({"event":event,"invocation_id":call,
            "session_id":"session","tool_call":{"name":"exec","id":call,"input":input.to_string()},
            "effective_input":input,"tool_response":{"content":[{"type":"text","text":"original"}],"state":"success"}}),
        "pi" => json!({"event":event,"invocation_id":call,"session_id":"session",
            "tool_call":{"name":"bash","id":call,"input":input},
            "tool_response":{"content":[{"type":"text","text":"original"}],"isError":false}}),
        "hermes" => json!({"hook_event_name":if before {"pre_tool_call"} else {"post_tool_call"},
            "cwd":cwd,"session_id":"session","tool_name":"terminal","tool_input":input,
            "extra":{"tool_call_id":call,"api_request_id":"api","status":"ok","duration_ms":3,
                "error_type":null,"error_message":null,"middleware_trace":[{"kept":true}],
                "result":"{\"output\":\"original\",\"exit_code\":0}"}}),
        _ => panic!("new adapter {adapter} needs a native regression fixture"),
    };
    if before {
        native.as_object_mut().unwrap().remove("tool_response");
        if adapter == "openclaw" {
            native["event"].as_object_mut().unwrap().remove("result");
        }
        if adapter == "hermes" {
            native["extra"] = json!({"tool_call_id":call,"api_request_id":"api"});
        }
    }
    native
}
