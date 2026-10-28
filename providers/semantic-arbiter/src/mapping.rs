//! Explicit lexical projection only; no filesystem identity or authorization attestation.

use semantic_arbiter::contract::{
    Binding, Budget, Candidate, NativeTool, Scenario, Task, TaskOrigin,
};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{EvaluationMode, EvaluationRequest, EVALUATION_API_VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::{derive, resolve, single_file_input, Config};

pub(crate) fn identity(domain: &str, value: &Value) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| "mapping_failed")?;
    let mut hash = Sha256::new();
    hash.update(b"aw-provider-semantic-arbiter/v1\0");
    hash.update(domain.as_bytes());
    hash.update(b"\0");
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

pub(crate) fn project(
    request: &Value,
    config: &Config,
    ingress: Ingress,
) -> Result<EvaluationRequest, &'static str> {
    let event = &request["event"];
    if request["operation"] != "evaluate_task_scope" || event["name"] != "tool.before" {
        return Err("unsupported_operation");
    }
    if event["session_id"] != config.task.session_id || event["agent"]["adapter"] != config.adapter
    {
        return Err("context_mismatch");
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
        &event["tool"]["call_id"],
    ] {
        if value.as_str().is_none_or(|s| s.trim().is_empty()) {
            return Err("missing_identity");
        }
    }
    let name = event["tool"]["native_name"]
        .as_str()
        .ok_or("unmapped_tool")?;
    let mapping = config.tools.get(name).ok_or("unmapped_tool")?;
    let input = &event["tool"]["input"];
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
    let candidate = Candidate {
        operation: mapping.operation,
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
    let session_id = identity("session", &event["session_id"])?;
    let config_revision = identity("aw-config-revision", &request["config_revision"])?;
    let opened_at_ms = ingress.now_ms().saturating_sub(ingress.elapsed_ms());
    // AW JSON Schema accepts integral decimal and exponent number forms.
    let duration = request["budget_ms"]
        .as_f64()
        .ok_or("invalid_budget")?
        .min(config.model.timeout_ms as f64) as u64;
    Ok(EvaluationRequest {
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
            task_version: config.task.version.clone(),
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
        budget: Budget {
            opened_at_ms,
            deadline_ms: opened_at_ms.saturating_add(duration),
            max_evidence_items: 0,
            // Even an empty evidence array occupies two JSON bytes.
            max_evidence_bytes: 2,
        },
        scenarios: vec![Scenario::TaskScope],
        task: Some(Task {
            session_id,
            version: config.task.version.clone(),
            origin: TaskOrigin::Operator,
            text: config.task.text.clone(),
            allowed_resource_ids: config.task.allowed_resource_ids.clone(),
            preserve_resource_ids: config.task.preserve_resource_ids.clone(),
        }),
        candidate,
        evidence: vec![],
        gaps: vec![],
    })
}
