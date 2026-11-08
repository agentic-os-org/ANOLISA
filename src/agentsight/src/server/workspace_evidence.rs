//! Historical workspace evidence extracted from successful ATIF tool calls.

use std::collections::HashMap;
use std::path::Path;

use agentsight_atif::{AtifTrajectory, Step, StepSource};
use chrono::{DateTime, Utc};
use serde_json::Value;

/// A workspace candidate whose operation is explicitly successful and ordered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceCandidate {
    pub(crate) path: String,
    pub(crate) step_index: usize,
    pub(crate) observed_at: DateTime<Utc>,
    pub(crate) evidence: &'static str,
}

/// Agent work starts at request start when available, otherwise at the Step timestamp.
pub(crate) fn step_start(step: &Step) -> Option<DateTime<Utc>> {
    let start = step
        .extra
        .as_ref()
        .and_then(|extra| extra.get("start_timestamp"))
        .and_then(Value::as_str)
        .or(step.timestamp.as_deref())?;
    DateTime::parse_from_rfc3339(start)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// Extract monotonically newer candidates without consulting process cwd or live plugin config.
pub(crate) fn workspace_candidates(document: &AtifTrajectory) -> Vec<WorkspaceCandidate> {
    let calls = successful_calls(document);
    let mut candidates: Vec<WorkspaceCandidate> = Vec::new();

    for (step_index, step) in document.steps.iter().enumerate() {
        if step.source != StepSource::Agent {
            continue;
        }
        let Some(observed_at) = step_start(step) else {
            continue;
        };
        for call in step.tool_calls.iter().flatten() {
            if calls.get(call.tool_call_id.as_str()) != Some(&true) {
                continue;
            }
            let Some((path, evidence)) =
                explicit_workspace(call.function_name.as_str(), &call.arguments)
            else {
                continue;
            };
            if !Path::new(&path).is_absolute() {
                continue;
            }
            let candidate = WorkspaceCandidate {
                path,
                step_index,
                observed_at,
                evidence,
            };
            if candidates.last().is_some_and(|previous| {
                candidate.observed_at < previous.observed_at
                    || (candidate.observed_at == previous.observed_at
                        && candidate.step_index <= previous.step_index)
            }) {
                continue;
            }
            if candidates
                .last()
                .is_some_and(|previous| previous.path == candidate.path)
            {
                candidates.pop();
            }
            candidates.push(candidate);
        }
    }
    candidates
}

fn successful_calls(document: &AtifTrajectory) -> HashMap<&str, bool> {
    let mut calls = HashMap::new();
    for result in document
        .steps
        .iter()
        .filter_map(|step| step.observation.as_ref())
        .flat_map(|observation| observation.results.iter())
    {
        let Some(call_id) = result.source_call_id.as_deref() else {
            continue;
        };
        let succeeded = observation_succeeded(result);
        calls
            .entry(call_id)
            .and_modify(|known| *known &= succeeded)
            .or_insert(succeeded);
    }
    calls
}

fn observation_succeeded(result: &agentsight_atif::ObservationResult) -> bool {
    let structured = result.content.as_ref().and_then(structured_success);
    match (result.is_error(), structured) {
        (Some(true), _) | (_, Some(false)) => false,
        (Some(false), _) | (None, Some(true)) => true,
        (None, None) => false,
    }
}

fn structured_success(content: &Value) -> Option<bool> {
    match content {
        Value::Object(object) => object.get("success").and_then(Value::as_bool),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .as_ref()
            .and_then(structured_success),
        _ => None,
    }
}

fn explicit_workspace(function_name: &str, arguments: &Value) -> Option<(String, &'static str)> {
    let normalized = function_name.to_ascii_lowercase().replace('_', "-");
    let parsed: Value;
    let arguments = match arguments {
        Value::Object(object) => object,
        Value::String(text) => {
            parsed = serde_json::from_str(text).ok()?;
            parsed.as_object()?
        }
        _ => return None,
    };
    if normalized == "ws-ckpt-config" {
        if arguments.get("action").and_then(Value::as_str) != Some("update")
            || arguments.get("key").and_then(Value::as_str) != Some("workspace")
        {
            return None;
        }
        return absolute_argument(arguments.get("value")).map(|path| (path, "config_update"));
    }
    if matches!(
        normalized.as_str(),
        "ws-ckpt-checkpoint"
            | "ws-ckpt-rollback"
            | "ws-ckpt-list"
            | "ws-ckpt-diff"
            | "ws-ckpt-delete"
            | "ws-ckpt-status"
    ) {
        return absolute_argument(arguments.get("workspace")).map(|path| (path, "tool_argument"));
    }
    if normalized == "bash" {
        return arguments
            .get("command")
            .and_then(Value::as_str)
            .and_then(bash_workspace)
            .map(|path| (path, "bash_argument"));
    }
    None
}

fn absolute_argument(value: Option<&Value>) -> Option<String> {
    let path = value?.as_str()?.trim();
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    Some(path.to_string())
}

fn bash_workspace(command: &str) -> Option<String> {
    if command.chars().any(|character| {
        matches!(
            character,
            '$' | '`' | ';' | '|' | '&' | '<' | '>' | '\n' | '\r' | '\0'
        )
    }) {
        return None;
    }
    let tokens = shell_words(command)?;
    if tokens.len() < 2
        || Path::new(tokens.first()?).file_name()?.to_str()? != "ws-ckpt"
        || !matches!(
            tokens.get(1)?.as_str(),
            "checkpoint" | "rollback" | "list" | "diff" | "status" | "delete"
        )
    {
        return None;
    }
    let mut workspace = None;
    let mut index = 2;
    while index < tokens.len() {
        let token = &tokens[index];
        let value = if matches!(token.as_str(), "-w" | "--workspace") {
            index += 1;
            tokens.get(index).map(String::as_str)
        } else {
            token.strip_prefix("--workspace=")
        };
        if let Some(value) = value {
            if workspace.is_some() || value.is_empty() || !Path::new(value).is_absolute() {
                return None;
            }
            workspace = Some(value.to_string());
        }
        index += 1;
    }
    workspace
}

fn shell_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in command.chars() {
        if escaped {
            word.push(character);
            escaped = false;
            started = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                word.push(character);
            }
            started = true;
            continue;
        }
        if character.is_whitespace() && quote.is_none() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
            continue;
        }
        word.push(character);
        started = true;
    }
    if escaped || quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    Some(words)
}
