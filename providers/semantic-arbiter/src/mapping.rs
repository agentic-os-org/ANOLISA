//! Explicit lexical projection only; no filesystem identity or authorization attestation.

use semantic_arbiter::contract::{
    Binding, Budget, Candidate, NativeTool, Operation, Task, TaskOrigin,
};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{EvaluationMode, EvaluationRequest, EVALUATION_API_VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::command;
use crate::config::{
    derive, resolve, single_file_input, CommandMapping, Config, FileMapping, ToolMapping,
    COMMAND_PREFIX, DERIVED_PREFIX,
};
use crate::session::{digest, CapturedTask};

/// Fields a mapped shell tool may carry besides the command itself.
const COMMAND_FIELDS: &[&str] = &[
    "command",
    "description",
    "timeout",
    "run_in_background",
    "dir_path",
];

pub(crate) fn identity(domain: &str, value: &Value) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| "mapping_failed")?;
    let mut hash = Sha256::new();
    hash.update(b"aw-provider-semantic-arbiter/v1\0");
    hash.update(domain.as_bytes());
    hash.update(b"\0");
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

/// Validates the context shared by both events and derives the opaque session.
///
/// # Errors
///
/// `unsupported_operation`, `context_mismatch` or `missing_identity`, in that order.
pub(crate) fn context(
    request: &Value,
    config: &Config,
    event_name: &str,
) -> Result<String, &'static str> {
    let event = &request["event"];
    if request["operation"] != "evaluate_task_scope" || event["name"] != event_name {
        return Err("unsupported_operation");
    }
    if event["agent"]["adapter"] != config.adapter {
        return Err("context_mismatch");
    }
    let native_session = event["session_id"].as_str().ok_or("missing_identity")?;
    if native_session.trim().is_empty()
        || native_session.len() > 512
        || native_session.chars().any(char::is_control)
    {
        return Err("missing_identity");
    }
    let cwd = std::env::current_dir().map_err(|_| "context_mismatch")?;
    if cwd.to_str() != Some(&config.workspace_path)
        || event["native"]["cwd"].as_str() != Some(&config.workspace_path)
    {
        return Err("context_mismatch");
    }
    for value in [
        &event["agent"]["instance_id"],
        &event["agent"]["binding_id"],
    ] {
        if value.as_str().is_none_or(|s| s.trim().is_empty()) {
            return Err("missing_identity");
        }
    }
    identity(
        "session",
        &json!([
            event["agent"]["adapter"],
            event["agent"]["binding_id"],
            event["agent"]["instance_id"],
            native_session
        ]),
    )
}

/// Projects one `tool.before` call; the session task is loaded only after the
/// call itself is known to be covered, so uncovered calls never read prompts.
pub(crate) fn project(
    request: &Value,
    config: &Config,
    ingress: Ingress,
    load_task: impl FnOnce(&str) -> Result<CapturedTask, &'static str>,
) -> Result<(EvaluationRequest, usize), &'static str> {
    let event = &request["event"];
    let session_id = context(request, config, "tool.before")?;
    if event["tool"]["call_id"]
        .as_str()
        .is_none_or(|s| s.trim().is_empty())
    {
        return Err("missing_identity");
    }
    let name = event["tool"]["native_name"]
        .as_str()
        .ok_or("unmapped_tool")?;
    let mapping = config.tools.get(name).ok_or("unmapped_tool")?;
    let input = &event["tool"]["input"];
    let (operation, resource_id) = match mapping {
        ToolMapping::File(file) => file_resource(name, file, input, config)?,
        ToolMapping::Command(shell) => command_resource(input, shell, config)?,
    };
    let candidate = Candidate {
        operation,
        resource_id,
        native_tool: Some(NativeTool {
            name: name.into(),
            input: input.clone(),
        }),
    };
    let candidate_revision =
        semantic_arbiter::contract::candidate_revision(&candidate).map_err(|error| {
            if error == semantic_arbiter::protocol::ContractError::MessageTooLarge {
                "oversized_candidate"
            } else {
                "candidate_invalid"
            }
        })?;
    let task = load_task(&session_id)?;
    let task_version = digest(&task.text);
    let config_revision = identity("aw-config-revision", &request["config_revision"])?;
    let evaluation = EvaluationRequest {
        api_version: EVALUATION_API_VERSION.into(),
        request_id: identity("request", &request["request_id"])?,
        mode: EvaluationMode::ModelEvaluation,
        binding: Binding {
            session_id: session_id.clone(),
            instance_id: identity(
                "instance",
                &json!([
                    event["agent"]["adapter"],
                    event["agent"]["binding_id"],
                    event["agent"]["instance_id"]
                ]),
            )?,
            workspace_id: config.workspace_id.clone(),
            task_version: task_version.clone(),
            config_revision,
            candidate_id: identity(
                "candidate",
                &json!([
                    event["agent"],
                    event["session_id"],
                    event["tool"]["call_id"]
                ]),
            )?,
            candidate_revision,
        },
        budget: budget(request, config, ingress)?,
        scenarios: config.scenarios.clone(),
        task: Some(Task {
            session_id,
            version: task_version,
            origin: TaskOrigin::NativeUser,
            text: task.text,
            allowed_resource_ids: config.task.allowed_resource_ids.clone(),
            preserve_resource_ids: config.task.preserve_resource_ids.clone(),
        }),
        candidate,
        evidence: vec![],
        gaps: vec![],
    };
    Ok((evaluation, task.turns))
}

fn file_resource(
    name: &str,
    mapping: &FileMapping,
    input: &Value,
    config: &Config,
) -> Result<(Operation, String), &'static str> {
    if !single_file_input(name, mapping, input) {
        return Err("unsupported_input");
    }
    let path = input
        .pointer(&mapping.path_pointer)
        .and_then(Value::as_str)
        .ok_or("unsupported_input")?;
    let resolved = resolve(path, &config.workspace_path).ok_or("unmapped_resource")?;
    let resource_id = match config.resources.get(&resolved) {
        Some(label) => label.clone(),
        None => derive(&resolved, &config.workspace_path).ok_or("unmapped_resource")?,
    };
    Ok((mapping.operation, resource_id))
}

/// Picks the one named target that decides the deterministic gate.
///
/// A preserved target wins, then one outside a declared allowance, then the
/// first named target. A command with no attributable target is projected as
/// `command:unresolved`, so the model judges the full call; under a declared
/// allowance that placeholder is outside scope unless the operator lists it.
fn command_resource(
    input: &Value,
    mapping: &CommandMapping,
    config: &Config,
) -> Result<(Operation, String), &'static str> {
    let fields = input.as_object().ok_or("unsupported_input")?;
    let text = input
        .pointer(&mapping.command_pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or("unsupported_input")?;
    if fields
        .keys()
        .any(|key| !COMMAND_FIELDS.contains(&key.as_str()))
    {
        return Err("unsupported_input");
    }
    let workspace = config.workspace_path.trim_end_matches('/');
    // Another run directory rebases every relative path the lexer reports.
    let rebased = match fields.get("dir_path") {
        None => false,
        Some(Value::String(dir)) => dir.trim_end_matches('/') != workspace,
        Some(_) => return Err("unsupported_input"),
    };
    let analysis = command::analyze(text);
    let mut unresolved = analysis.unresolved;
    let mut targets = vec![];
    for target in analysis.targets {
        if !target.path.starts_with('/') && (target.base_unknown || rebased) {
            unresolved = true;
            continue;
        }
        let Some(path) = locate(&target.path, workspace) else {
            unresolved = true;
            continue;
        };
        if path == workspace {
            targets.push((target.operation, None, path, true));
            continue;
        }
        match config
            .resources
            .get(&path)
            .cloned()
            .or_else(|| derive(&path, &config.workspace_path))
        {
            Some(id) => targets.push((target.operation, Some(id), path, target.tree)),
            None => unresolved = true,
        }
    }
    let task = &config.task;
    for (operation, id, path, tree) in &targets {
        if let Some(id) = id
            .as_ref()
            .filter(|id| task.preserve_resource_ids.contains(id))
        {
            return Ok((*operation, id.clone()));
        }
        if *tree {
            let below = format!("{path}/");
            let contained = task.preserve_resource_ids.iter().find(|kept| {
                resource_path(kept, config).is_some_and(|kept| kept.starts_with(&below))
            });
            if let Some(kept) = contained {
                return Ok((*operation, kept.clone()));
            }
        }
    }
    if !task.allowed_resource_ids.is_empty() {
        let outside = targets.iter().find(|(_, id, _, _)| {
            id.as_ref()
                .is_none_or(|id| !task.allowed_resource_ids.contains(id))
        });
        if let Some((operation, id, _, _)) = outside {
            return Ok((*operation, id.clone().unwrap_or_else(unresolved_id)));
        }
    }
    if let Some((operation, Some(id), _, _)) = targets.iter().find(|target| target.1.is_some()) {
        return Ok((*operation, id.clone()));
    }
    match targets.first() {
        Some((operation, ..)) => Ok((*operation, unresolved_id())),
        None if unresolved => Ok((Operation::Replace, unresolved_id())),
        None => Err("no_mutation"),
    }
}

fn unresolved_id() -> String {
    format!("{COMMAND_PREFIX}unresolved")
}

/// Normalizes `.` components and trailing slashes; the workspace itself is returned as-is.
fn locate(path: &str, workspace: &str) -> Option<String> {
    let parts: Vec<_> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let joined = parts.join("/");
    let candidate = if path.starts_with('/') {
        format!("/{joined}")
    } else if joined.is_empty() {
        return Some(workspace.to_owned());
    } else {
        format!("{workspace}/{joined}")
    };
    if candidate == workspace {
        return Some(candidate);
    }
    resolve(&candidate, workspace)
}

/// Workspace path of a listed resource, when the identity names one.
fn resource_path(id: &str, config: &Config) -> Option<String> {
    if let Some(relative) = id.strip_prefix(DERIVED_PREFIX) {
        let workspace = config.workspace_path.trim_end_matches('/');
        return Some(format!("{workspace}/{}", relative.replace(':', "/")));
    }
    config
        .resources
        .iter()
        .find(|(_, label)| *label == id)
        .map(|(path, _)| path.clone())
}

pub(crate) fn budget(
    request: &Value,
    config: &Config,
    ingress: Ingress,
) -> Result<Budget, &'static str> {
    let opened_at_ms = ingress.now_ms().saturating_sub(ingress.elapsed_ms());
    // AW JSON Schema accepts integral decimal and exponent number forms.
    let duration = request["budget_ms"]
        .as_f64()
        .ok_or("invalid_budget")?
        .min(config.model.timeout_ms as f64) as u64;
    Ok(Budget {
        opened_at_ms,
        deadline_ms: opened_at_ms.saturating_add(duration),
        max_evidence_items: 0,
        // Even an empty evidence array occupies two JSON bytes.
        max_evidence_bytes: 2,
    })
}
