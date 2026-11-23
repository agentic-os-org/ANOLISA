//! Execution evidence is distinct from abbreviated display arguments.
use crate::types::ToolCallRecord;

pub(crate) fn from_arguments(arguments: &serde_json::Value) -> Option<String> {
    ["command", "cmd", "script"].iter().find_map(|key| {
        let value = arguments.get(key)?;
        let command = if let Some(text) = value.as_str() {
            text.to_owned()
        } else {
            // Codex shell tools also record an argv array. Reject mixed arrays
            // instead of silently dropping arguments from the evidence.
            value
                .as_array()?
                .iter()
                .map(|part| part.as_str())
                .collect::<Option<Vec<_>>>()?
                .join(" ")
        };
        (!command.trim().is_empty()).then_some(command)
    })
}

impl ToolCallRecord {
    pub(crate) fn execution_command(&self) -> Option<String> {
        if let Some(command) = &self.command {
            return (!command.trim().is_empty()).then(|| command.clone());
        }
        // Older records carry no dedicated field. Complete argument objects
        // and legacy plain commands remain readable; broken JSON previews do
        // not become executable evidence just because parsing failed.
        if let Ok(arguments) = serde_json::from_str(&self.cmd) {
            return from_arguments(&arguments);
        }
        let text = self.cmd.trim_start();
        (self.target.is_none() && !text.is_empty() && !text.starts_with(['{', '[']))
            .then(|| self.cmd.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extraction_preserves_unicode_and_accepts_command_aliases() {
        for key in ["command", "cmd", "script"] {
            assert_eq!(
                from_arguments(&json!({key:"echo 验证 && cargo test"})).as_deref(),
                Some("echo 验证 && cargo test")
            );
        }
        assert_eq!(
            from_arguments(&json!({"command":["cargo","test","--workspace"]})).as_deref(),
            Some("cargo test --workspace")
        );
        for arguments in [
            json!({"command":["cargo",1]}),
            json!({"command":[]}),
            json!({"command":" "}),
            json!({"todos":["run tests"]}),
        ] {
            assert_eq!(from_arguments(&arguments), None);
        }
    }

    #[test]
    fn legacy_records_do_not_promote_broken_json_to_commands() {
        let make = |cmd| {
            serde_json::from_value::<ToolCallRecord>(
                json!({"name":"Bash","call_id":"call","start":0.0,"dur":0.0,"cmd":cmd,"err":false}),
            )
            .unwrap()
        };
        assert_eq!(
            make("cargo test").execution_command().as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            make(r#"{"command":"cargo test"}"#)
                .execution_command()
                .as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            make(r#"{"todos":[{"content":"run tests..."#).execution_command(),
            None
        );
        let mut record = make("truncated preview");
        record.command = Some("cargo test --workspace".into());
        assert_eq!(
            record.execution_command().as_deref(),
            Some("cargo test --workspace")
        );
        let serialized = serde_json::to_value(record).unwrap();
        assert!(
            serialized.get("command").is_none(),
            "performance summaries retain only their bounded preview"
        );
        let restored: ToolCallRecord = serde_json::from_value(serialized).unwrap();
        assert!(restored.command.is_none());
    }
}
