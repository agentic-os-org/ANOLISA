//! Offline, closed configuration: task permission never comes from the candidate.

use std::collections::{BTreeMap, BTreeSet};

use semantic_arbiter::config::ModelConfig;
use semantic_arbiter::contract::{Operation, Scenario};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::{open_root, CALL_LIMIT, TASK_LIMIT};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub version: u32,
    #[serde(default = "default_scenarios")]
    pub scenarios: Vec<Scenario>,
    pub task: TaskConfig,
    pub adapter: String,
    pub workspace_path: String,
    pub workspace_id: String,
    pub tools: BTreeMap<String, ToolMapping>,
    pub resources: BTreeMap<String, String>,
    pub model: ModelConfig,
    pub record_dir: String,
    pub max_sessions: u64,
    pub max_session_bytes: u64,
    pub max_storage_bytes: u64,
}

/// Operator resource constraints; the task text itself comes from `input.submit`.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskConfig {
    pub allowed_resource_ids: Vec<String>,
    pub preserve_resource_ids: Vec<String>,
}

fn default_scenarios() -> Vec<Scenario> {
    vec![Scenario::TaskScope]
}

/// Operator-declared input contract for one native tool name.
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum ToolMapping {
    File(FileMapping),
    Command(CommandMapping),
}

/// A tool whose input names exactly one file.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileMapping {
    pub operation: Operation,
    pub path_pointer: String,
}

/// A shell tool whose command string is analyzed lexically.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandMapping {
    pub command_pointer: String,
}

impl Config {
    pub(crate) fn decode(value: &Value) -> Result<Self, &'static str> {
        let config: Self = serde_json::from_value(value.clone()).map_err(|_| "invalid_config")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), &'static str> {
        let repeated = (1..self.scenarios.len())
            .any(|index| self.scenarios[..index].contains(&self.scenarios[index]));
        if self.scenarios.is_empty() || repeated {
            return Err("invalid_config");
        }
        // Preservation needs local evidence this Provider does not collect yet;
        // refuse it at startup rather than silently evaluating task scope only.
        if self.scenarios.contains(&Scenario::Preservation) {
            return Err("unsupported_scenario");
        }
        if self.version != 1
            || !["qoder", "openclaw", "hermes", "qwenpaw", "pi", "claude-sdk", "qoder-sdk", "agentscope"]
                .contains(&self.adapter.as_str())
            || !id(&self.workspace_id)
            || !absolute(&self.workspace_path)
            || !absolute(&self.record_dir)
            || !(1..=10_000).contains(&self.max_sessions)
            // A full session task plus one admitted call must fit both byte limits.
            || !(CALL_LIMIT + TASK_LIMIT..=1024 * 1024 * 1024).contains(&self.max_storage_bytes)
            || !(CALL_LIMIT + TASK_LIMIT..=self.max_storage_bytes).contains(&self.max_session_bytes)
            || self.tools.is_empty()
            || self.tools.len() > 6
            || self.resources.len() > 64
            // The AW host revision includes this config; use an explicit marker,
            // not an impossible self-referential hash or an unrelated revision.
            || self.model.config_revision != "aw_request"
        {
            return Err("invalid_config");
        }
        self.model.validate().map_err(|_| "invalid_config")?;
        for (name, mapping) in &self.tools {
            let valid = match mapping {
                ToolMapping::File(file) => {
                    let operation = match name.as_str() {
                        "Edit" | "Write" | "edit" | "write" => Operation::Replace,
                        "Delete" | "delete" => Operation::Delete,
                        _ => return Err("invalid_config"),
                    };
                    file.operation == operation
                        && ["/file_path", "/path"].contains(&file.path_pointer.as_str())
                }
                ToolMapping::Command(command) => {
                    ["Bash", "bash"].contains(&name.as_str())
                        && command.command_pointer == "/command"
                }
            };
            if !valid {
                return Err("invalid_config");
            }
        }
        let mut seen = BTreeSet::new();
        for (path, resource) in &self.resources {
            // Explicit labels must stay outside the derived namespace so a label
            // can never collide with the derived identity of a different path.
            if !within(path, &self.workspace_path)
                || !id(resource)
                || resource.starts_with(DERIVED_PREFIX)
                || resource.starts_with(COMMAND_PREFIX)
                || !seen.insert(resource)
            {
                return Err("invalid_config");
            }
        }
        for values in [
            &self.task.allowed_resource_ids,
            &self.task.preserve_resource_ids,
        ] {
            let unique: BTreeSet<_> = values.iter().collect();
            // A listed identity may be an explicit label or a derived path form,
            // and derived identities cannot be enumerated ahead of an event.
            if values.len() > 64
                || unique.len() != values.len()
                || values.iter().any(|value| !id(value))
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

/// Namespace reserved for identities derived from a workspace-relative path.
pub(crate) const DERIVED_PREFIX: &str = "path:";

/// Namespace reserved for shell writes the lexer cannot attribute to a named path.
pub(crate) const COMMAND_PREFIX: &str = "command:";

/// Derive a readable resource identity so an operator need not enumerate resources.
pub(crate) fn derive(resolved: &str, workspace: &str) -> Option<String> {
    let relative = resolved
        .strip_prefix(workspace.trim_end_matches('/'))?
        .trim_start_matches('/');
    // The separator is ':', so a literal ':' would make the mapping ambiguous.
    if relative.is_empty() || relative.contains(':') {
        return None;
    }
    let derived = format!("{DERIVED_PREFIX}{}", relative.replace('/', ":"));
    id(&derived).then_some(derived)
}

pub(crate) fn single_file_input(name: &str, mapping: &FileMapping, input: &Value) -> bool {
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
