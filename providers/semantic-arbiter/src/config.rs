//! Offline, closed configuration: task permission never comes from the candidate.

use std::collections::{BTreeMap, BTreeSet};

use semantic_arbiter::{config::ModelConfig, contract::Operation};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::{open_root, RESERVE_BYTES};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub version: u32,
    pub task: TaskConfig,
    pub adapter: String,
    pub workspace_path: String,
    pub workspace_id: String,
    pub tools: BTreeMap<String, ToolMapping>,
    pub resources: BTreeMap<String, String>,
    pub model: ModelConfig,
    pub record_dir: String,
    pub max_records: u64,
    pub max_storage_bytes: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskConfig {
    pub session_id: String,
    pub version: String,
    pub text: String,
    pub allowed_resource_ids: Vec<String>,
    pub preserve_resource_ids: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolMapping {
    pub operation: Operation,
    pub path_pointer: String,
}

impl Config {
    pub(crate) fn decode(value: &Value) -> Result<Self, &'static str> {
        let config: Self = serde_json::from_value(value.clone()).map_err(|_| "invalid_config")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || !["qoder", "openclaw", "hermes", "qwenpaw", "pi", "claude-sdk", "qoder-sdk", "agentscope"]
                .contains(&self.adapter.as_str())
            || !text(&self.task.session_id, 512)
            || !id(&self.task.version)
            || !id(&self.workspace_id)
            || self.task.text.trim().is_empty()
            || self.task.text.len() > 16_384
            || !absolute(&self.workspace_path)
            || !absolute(&self.record_dir)
            || !(1..=10_000).contains(&self.max_records)
            || !(RESERVE_BYTES..=1024 * 1024 * 1024).contains(&self.max_storage_bytes)
            || self.tools.is_empty()
            || self.tools.len() > 6
            || self.resources.is_empty()
            || self.resources.len() > 64
            // The AW host revision includes this config; use an explicit marker,
            // not an impossible self-referential hash or an unrelated revision.
            || self.model.config_revision != "aw_request"
        {
            return Err("invalid_config");
        }
        self.model.validate().map_err(|_| "invalid_config")?;
        for (name, mapping) in &self.tools {
            let operation = match name.as_str() {
                "Edit" | "Write" | "edit" | "write" => Operation::Replace,
                "Delete" | "delete" => Operation::Delete,
                _ => return Err("invalid_config"),
            };
            if mapping.operation != operation
                || !["/file_path", "/path"].contains(&mapping.path_pointer.as_str())
            {
                return Err("invalid_config");
            }
        }
        let mut seen = BTreeSet::new();
        for (path, resource) in &self.resources {
            if !within(path, &self.workspace_path) || !id(resource) || !seen.insert(resource) {
                return Err("invalid_config");
            }
        }
        for values in [
            &self.task.allowed_resource_ids,
            &self.task.preserve_resource_ids,
        ] {
            let unique: BTreeSet<_> = values.iter().collect();
            if values.len() > 64
                || unique.len() != values.len()
                || values
                    .iter()
                    .any(|value| !id(value) || !seen.contains(value))
            {
                return Err("invalid_config");
            }
        }
        // Opening and statting do not mutate the directory or read credentials.
        open_root(&self.record_dir).map_err(|_| "invalid_record_dir")?;
        Ok(())
    }
}

fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@".contains(&b))
}

fn text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

pub(crate) fn absolute(path: &str) -> bool {
    path.starts_with('/') && lexical(path)
}

fn lexical(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.contains('\\')
        && (path == "/"
            || path
                .trim_start_matches('/')
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."))
        && !path.starts_with("//")
}

fn within(path: &str, workspace: &str) -> bool {
    absolute(path)
        && path != workspace
        && (workspace == "/"
            || path
                .strip_prefix(workspace)
                .is_some_and(|rest| rest.starts_with('/')))
}

pub(crate) fn resolve(path: &str, workspace: &str) -> Option<String> {
    if !lexical(path) {
        return None;
    }
    let resolved = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{path}", workspace.trim_end_matches('/'))
    };
    within(&resolved, workspace).then_some(resolved)
}

pub(crate) fn single_file_input(name: &str, mapping: &ToolMapping, input: &Value) -> bool {
    let Some(fields) = input.as_object() else {
        return false;
    };
    let path_key = &mapping.path_pointer[1..];
    if !input
        .pointer(&mapping.path_pointer)
        .is_some_and(Value::is_string)
    {
        return false;
    }
    if mapping.operation == Operation::Delete {
        return fields.len() == 1;
    }
    let edit = name.eq_ignore_ascii_case("edit");
    let allowed: &[&str] = if edit {
        &["old_string", "new_string", "replace_all", "allow_multiple"]
    } else {
        &["content"]
    };
    fields
        .keys()
        .all(|key| key == path_key || allowed.contains(&key.as_str()))
        && if edit {
            input["old_string"].is_string()
                && input["new_string"].is_string()
                && ["replace_all", "allow_multiple"]
                    .iter()
                    .all(|key| fields.get(*key).is_none_or(Value::is_boolean))
        } else {
            input["content"].is_string()
        }
}
