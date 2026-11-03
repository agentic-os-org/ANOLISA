//! A standalone stdio Provider example; the AW library starts no process.

use aw_provider::{Protocol, MAX_MESSAGE_BYTES, VERSION};
use serde_json::{json, Value};
use std::io::{Read, Write};

fn configured_tools(value: &Value) -> Option<Vec<&str>> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object
        .get("blocked_tools")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protocol = Protocol::new()?;
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut input)?;
    let request = protocol.parse_request(&input)?;
    let value = request.as_value();
    let mut reply = json!({
        "api_version": VERSION, "request_id": value["request_id"], "status": "ok"
    });
    match value["method"].as_str() {
        Some("describe") => {
            reply["operations"] = json!([{
                "name": "check", "events": ["tool.before", "tool.after"],
                "effects": ["observe", "block"]
            }]);
        }
        Some("validate_config") if configured_tools(&value["config"]).is_some() => {}
        Some("invoke") => {
            let allowed = value["allowed_effects"].as_array();
            let tools = configured_tools(&value["config"]);
            if value["operation"] != "check" || tools.is_none() {
                reply["status"] = json!("error");
                reply["error_code"] = json!("invalid_operation_or_config");
            } else {
                let blocked = value["event"]["name"] == "tool.before"
                    && tools.is_some_and(|tools| {
                        tools
                            .iter()
                            .any(|name| value["event"]["tool"]["native_name"] == *name)
                    });
                if blocked && !allowed.is_some_and(|effects| effects.contains(&json!("block"))) {
                    reply["status"] = json!("error");
                    reply["error_code"] = json!("block_not_admitted");
                } else {
                    reply["input_digest"] = value["input_digest"].clone();
                    reply["effects"] = if blocked {
                        json!([{"type": "block", "reason_code": "tool_policy"}])
                    } else {
                        json!([])
                    };
                }
            }
        }
        _ => {
            reply["status"] = json!("error");
            reply["error_code"] = json!("invalid_config");
        }
    }
    // Exit zero even for a policy block or a structured method failure.
    // Only malformed transport input or an I/O failure exits nonzero.
    serde_json::to_writer(std::io::stdout().lock(), &reply)?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}
