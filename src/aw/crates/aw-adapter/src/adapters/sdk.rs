//! Public semantic callbacks for application-owned runtimes without a native dialect.

use crate::{Adapter, HookOutput, LaunchInput, NativeContext, NativeExit, PreparedLaunch, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;

/// Versioned semantic ingress for application-owned callbacks.
pub(crate) struct Sdk;

#[derive(Deserialize)]
struct Envelope {
    api_version: String,
    session_id: String,
    #[serde(flatten)]
    event: Event,
}

#[derive(Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Event {
    #[serde(rename = "input.submit")]
    Input { turn_id: String, text: String },
    #[serde(rename = "tool.before")]
    Before {
        call_id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool.after")]
    After {
        call_id: String,
        name: String,
        input: Value,
        result: Value,
    },
}

fn identity(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 512 {
        return Err("SDK semantic identities must be nonempty and at most 512 bytes".into());
    }
    Ok(())
}

impl Adapter for Sdk {
    fn prepare(&self, _: LaunchInput, _: &AtomicBool) -> Result<Box<dyn PreparedLaunch>> {
        Err("sdk requires application-owned registration and installed semantic callbacks".into())
    }

    fn normalize(&self, context: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        let envelope: Envelope = serde_json::from_value(native.clone())?;
        if envelope.api_version != "aw-hook-source/v1" || native["event"] != event {
            return Err("SDK semantic callback protocol or event differs from its route".into());
        }
        identity(&envelope.session_id)?;
        let mut output = json!({"name":event,"session_id":envelope.session_id,
            "agent":{"adapter":"sdk","binding_id":context.target,"instance_id":context.instance_id},
            "native":native});
        match envelope.event {
            Event::Input { turn_id, text } => {
                identity(&turn_id)?;
                output["turn_id"] = json!(turn_id);
                output["input"] = json!({"text":text});
            }
            event => {
                let (call_id, name, input, result) = match event {
                    Event::Before {
                        call_id,
                        name,
                        input,
                    } => (call_id, name, input, Value::Null),
                    Event::After {
                        call_id,
                        name,
                        input,
                        result,
                    } => (call_id, name, input, result),
                    Event::Input { .. } => unreachable!("input handled above"),
                };
                identity(&call_id)?;
                identity(&name)?;
                if !input.is_object() {
                    return Err("SDK semantic tool input must be an object".into());
                }
                output["tool"] = json!({"call_id":call_id,"name":name,"native_name":name,
                    "input":input,"result":result});
            }
        }
        Ok(output)
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: NativeExit::Code(if blocked { 2 } else { 0 }),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_events_bind_server_identity_and_reject_private_fields() {
        let context = NativeContext {
            target: "app".into(),
            instance_id: "owned".into(),
            cwd: "/tmp".into(),
        };
        let before = json!({"api_version":"aw-hook-source/v1","event":"tool.before",
            "session_id":"session","call_id":"call","name":"bash","input":{"command":"true"}});
        let result = Sdk.normalize(&context, "tool.before", &before).unwrap();
        assert_eq!(result["agent"]["instance_id"], "owned");
        assert_eq!(result["tool"]["input"], before["input"]);
        for (field, value) in [
            ("instance_id", json!("foreign")),
            ("aw_source", json!("verified")),
            ("api_version", json!("v0")),
            ("call_id", json!("")),
            ("input", json!([])),
        ] {
            let mut invalid = before.clone();
            invalid[field] = value;
            assert!(
                Sdk.normalize(&context, "tool.before", &invalid).is_err(),
                "{field}"
            );
        }
        assert!(Sdk.normalize(&context, "tool.after", &before).is_err());
        let mut after = before.clone();
        after["result"] = json!({"text":"done"});
        after["event"] = json!("tool.after");
        assert_eq!(
            Sdk.normalize(&context, "tool.after", &after).unwrap()["tool"]["result"],
            json!({"text":"done"})
        );
    }
    #[test]
    fn semantic_profile_rejects_unverified_effects_and_protocols() {
        let document = json!({"spec":{"agents":{"app":{"adapter":"sdk"}},
            "execution":{"default_event_budget_ms":1000}}});
        let mut capabilities =
            crate::adapter::catalog::SDK.capabilities("aw-hook-source/v1", "application");
        assert!(crate::reload::scope_policy(&document, "app", &[], &capabilities).is_ok());
        capabilities
            .events
            .get_mut("tool.after")
            .unwrap()
            .push("replace_result".into());
        assert!(crate::reload::scope_policy(&document, "app", &[], &capabilities).is_err());
        capabilities.events.get_mut("tool.after").unwrap().pop();
        capabilities.version = "v0".into();
        assert!(crate::reload::scope_policy(&document, "app", &[], &capabilities).is_err());
        capabilities.version = "aw-hook-source/v1".into();
        capabilities
            .failure_actions
            .insert("tool.after".into(), vec!["withhold_result".into()]);
        assert!(crate::reload::scope_policy(&document, "app", &[], &capabilities).is_err());
    }
}
