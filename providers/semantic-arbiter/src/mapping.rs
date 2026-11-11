//! Explicit lexical projection only; no filesystem identity or authorization attestation.

use semantic_arbiter::contract::{
    Binding, Budget, Candidate, Change, NativeTool, Operation, Task, TaskOrigin,
    MAX_CANDIDATE_CHANGES,
};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{EvaluationMode, EvaluationRequest, EVALUATION_API_VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::command;
use crate::config::{
    derive, resolve, single_file_input, CommandMapping, Config, FileMapping, PatchMapping,
    ToolMapping, COMMAND_PREFIX, DERIVED_PREFIX,
};
use crate::patch::{self, Step};
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
    let changes = match mapping {
        ToolMapping::File(file) => vec![file_change(name, file, input, config)?],
        ToolMapping::Command(shell) => command_changes(input, shell, config)?,
        ToolMapping::Patch(patch) => patch_changes(input, patch, config)?,
    };
    if changes.len() > MAX_CANDIDATE_CHANGES {
        return Err("oversized_candidate");
    }
    let candidate = Candidate {
        changes,
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

fn file_change(
    name: &str,
    mapping: &FileMapping,
    input: &Value,
    config: &Config,
) -> Result<Change, &'static str> {
    if !single_file_input(name, mapping, input) {
        return Err("unsupported_input");
    }
    let path = input
        .pointer(&mapping.path_pointer)
        .and_then(Value::as_str)
        .ok_or("unsupported_input")?;
    let resolved = resolve(path, &config.workspace_path).ok_or("unmapped_resource")?;
    Ok(Change {
        operation: mapping.operation,
        resource_id: identify(&resolved, config).ok_or("unmapped_resource")?,
    })
}

/// Projects every named target, in command order, as one change.
///
/// A recursive target also changes each preserved resource below it, so the
/// gate blocks it without the model. A target the lexer cannot attribute
/// (scripts, interpreters, unlisted programs, substitutions, variables, the
/// workspace itself, or a relative path after `cd` or under another
/// `dir_path`) becomes `command:unresolved`, which the model judges from the
/// full call; under a declared allowance it is outside scope unless listed.
fn command_changes(
    input: &Value,
    mapping: &CommandMapping,
    config: &Config,
) -> Result<Vec<Change>, &'static str> {
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
    let mut changes = vec![];
    for target in analysis.targets {
        let operation = target.operation;
        let located = if !target.path.starts_with('/') && (target.base_unknown || rebased) {
            None
        } else {
            locate(&target.path, workspace)
        };
        let Some(path) = located else {
            add(&mut changes, operation, unresolved_id());
            continue;
        };
        let whole = path == workspace;
        let id = if whole { None } else { identify(&path, config) };
        add(&mut changes, operation, id.unwrap_or_else(unresolved_id));
        if target.tree || whole {
            let below = format!("{path}/");
            for kept in &config.task.preserve_resource_ids {
                if resource_path(kept, config).is_some_and(|kept| kept.starts_with(&below)) {
                    add(&mut changes, operation, kept.clone());
                }
            }
        }
    }
    if analysis.unresolved {
        add(&mut changes, Operation::Replace, unresolved_id());
    }
    if changes.is_empty() {
        return Err("no_mutation");
    }
    Ok(changes)
}

/// Projects every hunk of an OpenClaw patch; any unnamed file leaves the call uncovered.
fn patch_changes(
    input: &Value,
    mapping: &PatchMapping,
    config: &Config,
) -> Result<Vec<Change>, &'static str> {
    let text = input
        .pointer(&mapping.patch_pointer)
        .and_then(Value::as_str)
        .ok_or("unsupported_input")?;
    if input.as_object().is_none_or(|fields| fields.len() != 1) {
        return Err("unsupported_input");
    }
    let steps = patch::parse(text).ok_or("unsupported_input")?;
    let workspace = config.workspace_path.trim_end_matches('/');
    let named = |path: &str| {
        // OpenClaw resolves `@` against existing files and `~` against home.
        if path.starts_with(['@', '~']) {
            return None;
        }
        locate(path, workspace)
            .filter(|path| path != workspace)
            .and_then(|path| identify(&path, config))
    };
    let mut changes = vec![];
    for step in steps {
        match step {
            Step::Write(path) => add(
                &mut changes,
                Operation::Replace,
                named(&path).ok_or("unmapped_resource")?,
            ),
            Step::Remove(path) => add(
                &mut changes,
                Operation::Delete,
                named(&path).ok_or("unmapped_resource")?,
            ),
            Step::Move { from, to } => {
                let from = named(&from).ok_or("unmapped_resource")?;
                let to = named(&to).ok_or("unmapped_resource")?;
                // A move onto its own source only rewrites the file.
                if from != to {
                    add(&mut changes, Operation::Replace, to);
                    add(&mut changes, Operation::Delete, from);
                } else {
                    add(&mut changes, Operation::Replace, to);
                }
            }
        }
    }
    Ok(changes)
}

/// Names each resource once; a resource any step deletes is reported as deleted.
fn add(changes: &mut Vec<Change>, operation: Operation, resource_id: String) {
    match changes
        .iter_mut()
        .find(|change| change.resource_id == resource_id)
    {
        Some(change) if operation == Operation::Delete => change.operation = operation,
        Some(_) => {}
        None => changes.push(Change {
            operation,
            resource_id,
        }),
    }
}

/// Explicit label of a workspace path, else its derived identity.
fn identify(path: &str, config: &Config) -> Option<String> {
    config
        .resources
        .get(path)
        .cloned()
        .or_else(|| derive(path, &config.workspace_path))
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn openclaw(records: &std::path::Path) -> Config {
        Config::decode(&json!({
            "version":1,
            "task":{"allowed_resource_ids":[],"preserve_resource_ids":["greeting"]},
            "adapter":"openclaw","workspace_path":"/work","workspace_id":"workspace",
            "tools":{"apply_patch":{"patch_pointer":"/input"}},
            "resources":{"/work/greeting.txt":"greeting","/work/private.txt":"private"},
            "model":{"config_revision":"aw_request","backend":{"kind":"bailian",
                "endpoint_url":"http://127.0.0.1:1/unused","model":"unused","key_env":"UNUSED"},
                "timeout_ms":2000,"max_response_bytes":4096,"transport":"loopback_test"},
            "record_dir":records,"max_sessions":1,"max_session_bytes":88080384u64,
            "max_storage_bytes":88080384u64
        }))
        .unwrap()
    }

    fn changes(patch: &str) -> Result<Vec<(Operation, String)>, &'static str> {
        let records = tempfile::tempdir().unwrap();
        std::fs::set_permissions(records.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = openclaw(records.path());
        let ToolMapping::Patch(mapping) = &config.tools["apply_patch"] else {
            unreachable!("the fixture maps apply_patch as a patch tool");
        };
        let changes = patch_changes(&json!({ "input": patch }), mapping, &config)?;
        Ok(changes
            .into_iter()
            .map(|change| (change.operation, change.resource_id))
            .collect())
    }

    fn patch(hunks: &str) -> String {
        format!("*** Begin Patch\n{hunks}\n*** End Patch")
    }

    #[test]
    fn every_hunk_becomes_one_change_per_resource() {
        let multi = patch(
            "*** Add File: notes/new.txt\n+hello\n*** Update File: greeting.txt\n@@\n-a\n+b\n\
             *** Delete File: /work/private.txt\n*** Update File: a.txt\n*** Move to: b.txt\n-x\n+y",
        );
        let replace = |id: &str| (Operation::Replace, id.to_owned());
        let delete = |id: &str| (Operation::Delete, id.to_owned());
        assert_eq!(
            changes(&multi),
            Ok(vec![
                replace("path:notes:new.txt"),
                replace("greeting"),
                delete("private"),
                replace("path:b.txt"),
                delete("path:a.txt"),
            ])
        );
        // A move onto its own source rewrites the file in place.
        let onto_itself =
            patch("*** Update File: ./greeting.txt\n*** Move to: greeting.txt\n-a\n+b");
        assert_eq!(changes(&onto_itself), Ok(vec![replace("greeting")]));
        // Deleting then re-adding still names the resource once, as deleted.
        let readd = patch("*** Delete File: greeting.txt\n*** Add File: greeting.txt\n+new");
        assert_eq!(changes(&readd), Ok(vec![delete("greeting")]));
    }

    #[test]
    fn any_file_without_a_workspace_identity_leaves_the_patch_uncovered() {
        for path in [
            "@greeting.txt",
            "~/greeting.txt",
            "../outside.txt",
            "/etc/passwd",
            ".",
        ] {
            let hunks = format!("*** Add File: notes/new.txt\n+x\n*** Delete File: {path}");
            assert_eq!(changes(&patch(&hunks)), Err("unmapped_resource"), "{path}");
        }
    }

    #[test]
    fn malformed_or_extended_input_is_unsupported() {
        let records = tempfile::tempdir().unwrap();
        std::fs::set_permissions(records.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = openclaw(records.path());
        let ToolMapping::Patch(mapping) = &config.tools["apply_patch"] else {
            unreachable!("the fixture maps apply_patch as a patch tool");
        };
        let valid = patch("*** Delete File: private.txt");
        for input in [
            json!({"input":"*** Begin Patch\n*** Delete File: private.txt"}),
            json!({"input":valid,"cwd":"/elsewhere"}),
            json!({"input":["*** Begin Patch"]}),
            json!({"patch":valid}),
        ] {
            assert_eq!(
                patch_changes(&input, mapping, &config).map(|_| ()),
                Err("unsupported_input"),
                "{input}"
            );
        }
    }
}
