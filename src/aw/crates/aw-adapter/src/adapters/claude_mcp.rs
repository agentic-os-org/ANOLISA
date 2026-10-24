//! Public in-process MCP handlers have explicit task identity and a reliable error flag.

use crate::{
    adapter::{HookOutput, NativeContext},
    Exit, Result,
};
use serde_json::{json, Value};

pub(crate) const SOURCE: &str = "claude-sdk-mcp/v1";

pub(super) fn normalize(binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
    let object = native.as_object().ok_or("MCP callback must be an object")?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "aw_source"
                | "event"
                | "scope_id"
                | "invocation_id"
                | "server"
                | "tool_name"
                | "tool_input"
                | "tool_result"
                | "is_error"
        )
    }) || native["aw_source"] != SOURCE
        || native["event"] != event
        || native["scope_id"] != binding.instance_id
        || !native["tool_input"].is_object()
    {
        return Err("MCP callback source, scope or shape mismatch".into());
    }
    let identifier = |key: &str| -> Result<&str> {
        native[key]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            })
            .ok_or_else(|| "MCP server/tool identifiers must use bounded ASCII names".into())
    };
    let server = identifier("server")?;
    let tool = identifier("tool_name")?;
    let invocation = native["invocation_id"]
        .as_str()
        .ok_or("MCP invocation ID missing")?;
    if !super::sdk_tools::invocation_uuid(invocation) {
        return Err("MCP invocation requires a canonical UUID v4".into());
    }
    match event {
        "tool.before"
            if !object.contains_key("tool_result") && !object.contains_key("is_error") => {}
        "tool.after" if result_error(native).is_some() => {}
        _ => return Err("MCP phase/result mismatch".into()),
    }
    let name = format!("mcp__{server}__{tool}");
    Ok(json!({"name":event,
        "agent":{"adapter":"claude-sdk","binding_id":binding.target,"instance_id":binding.instance_id},
        "session_id":binding.instance_id,
        "tool":{"name":name,"native_name":name,"call_id":invocation,"input":native["tool_input"],"result":native.get("tool_result").cloned().unwrap_or(Value::Null)},
        "native":native}))
}

fn result_error(native: &Value) -> Option<bool> {
    let result = native["tool_result"].as_object()?;
    if result.contains_key("isError") {
        return None;
    }
    let error = match result.get("is_error") {
        None => false,
        Some(value) => value.as_bool()?,
    };
    (native["is_error"].as_bool()? == error).then_some(error)
}

pub(crate) fn project(native: &Value) -> Option<Value> {
    if native["aw_source"] != SOURCE
        || native["event"] != "tool.after"
        || result_error(native) != Some(false)
    {
        return None;
    }
    let result = native["tool_result"].as_object()?;
    if result.contains_key("structuredContent") {
        return None;
    }
    let blocks = result.get("content")?.as_array()?;
    if blocks.len() != 1 || blocks[0]["type"] != "text" {
        return None;
    }
    Some(
        json!({"text":blocks[0]["text"].as_str()?,"status":"success","content_origin":"api_response"}),
    )
}

pub(crate) fn reply(native: &Value, text: &str) -> Result<HookOutput> {
    project(native).ok_or("MCP result has no authorized text slot")?;
    let mut result = native["tool_result"].clone();
    result["content"][0]["text"] = json!(text);
    Ok(HookOutput {
        exit: Exit::Code(0),
        stdout: serde_json::to_vec(&json!({"aw_mcp_result":result}))?,
        stderr: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_source_keeps_scope_identity_and_only_successful_plain_text_is_replaceable() {
        let binding = NativeContext {
            instance_id: "task".into(),
            target: "claude".into(),
            cwd: "/work".into(),
        };
        let mut native = json!({"aw_source":SOURCE,"event":"tool.before","scope_id":"task",
            "invocation_id":"6b9a4d47-d3d1-4c08-a9e4-4ac72b1dd842","server":"business","tool_name":"run_script","tool_input":{"command":"opaque lookup"}});
        let before = normalize(&binding, "tool.before", &native).unwrap();
        assert_eq!(before["session_id"], "task");
        assert_eq!(crate::callback::correlation_source(&before), Some(SOURCE));
        assert_eq!(before["tool"]["name"], "mcp__business__run_script");
        assert!(before["native"].get("tool_use_id").is_none());
        native["event"] = json!("tool.after");
        native["is_error"] = json!(false);
        native["tool_result"] = json!({"content":[{"type":"text","text":"original","annotations":{"audience":["assistant"]}}],"metadata":{"keep":true}});
        let after = normalize(&binding, "tool.after", &native).unwrap();
        assert_eq!(before["tool"]["call_id"], after["tool"]["call_id"]);
        assert_eq!(
            project(&native).unwrap(),
            json!({"text":"original","status":"success","content_origin":"api_response"})
        );
        let changed: Value =
            serde_json::from_slice(&reply(&native, "compressed").unwrap().stdout).unwrap();
        let mut expected = native["tool_result"].clone();
        expected["content"][0]["text"] = json!("compressed");
        assert_eq!(changed["aw_mcp_result"], expected);
        for (field, value) in [
            ("is_error", json!(true)),
            (
                "tool_result",
                json!({"content":[{"type":"text","text":"x"}],"structuredContent":{}}),
            ),
            (
                "tool_result",
                json!({"content":[{"type":"text","text":"x"},{"type":"text","text":"y"}]}),
            ),
        ] {
            let mut unsupported = native.clone();
            unsupported[field] = value;
            assert!(project(&unsupported).is_none());
            assert!(reply(&unsupported, "compressed").is_err());
        }
        let mut forged = native.clone();
        forged["tool_result"]["is_error"] = json!(true);
        assert!(normalize(&binding, "tool.after", &forged).is_err());
        assert!(project(&forged).is_none());
        native["scope_id"] = json!("another-task");
        assert!(normalize(&binding, "tool.after", &native).is_err());
        native["scope_id"] = json!("task");
        native["tool_use_id"] = json!("invented");
        assert!(normalize(&binding, "tool.after", &native).is_err());
    }
}
