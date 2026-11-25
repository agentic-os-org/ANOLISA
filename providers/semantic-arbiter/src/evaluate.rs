//! Persist model-bound input before network I/O; expose only advisory local results.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use semantic_arbiter::contract::{EvidenceGap, Scenario};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{validate_response, EvaluationRequest, Evaluator};
use semantic_arbiter::evidence::{EvidenceRequest, EvidenceSourceRegistry};
use semantic_arbiter::model::{
    http::HttpBackend, BackendIdentity, ModelBackend, ModelError, ModelReply, ModelRequest,
};
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::verifier::{GitRebuildSource, SOURCE_ID};
use crate::{config::Config, mapping, observation, session, signals, storage::Record};

struct RecordingBackend {
    config: semantic_arbiter::config::ModelConfig,
    record: Arc<Record>,
    recording_failed: Arc<AtomicBool>,
}

struct PendingReply<'a> {
    backend: &'a RecordingBackend,
    completed: bool,
}

impl Drop for PendingReply<'_> {
    fn drop(&mut self) {
        // Evaluator timeout can cancel the wrapper before its own timeout fires.
        // Persist that sanitized failure unless the process itself was killed.
        if !self.completed
            && self
                .backend
                .record
                .save(
                    "model_result",
                    &json!({"status":"error","error":ModelError::Timeout}),
                )
                .is_err()
        {
            self.backend.recording_failed.store(true, Ordering::Relaxed);
        }
    }
}

impl ModelBackend for RecordingBackend {
    fn identity(&self) -> BackendIdentity {
        self.config.identity()
    }

    fn infer<'a>(
        &'a self,
        request: &'a ModelRequest,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<ModelReply, ModelError>> + Send + 'a>> {
        Box::pin(async move {
            if self.record.save("model_request", &json!({
                "backend": self.identity(), "state": request.state, "questions": request.questions
            })).is_err() {
                self.recording_failed.store(true, Ordering::Relaxed);
                return Err(ModelError::InvalidResponse);
            }
            let mut pending = PendingReply {
                backend: self,
                completed: false,
            };
            let result = match HttpBackend::from_env(&self.config) {
                Ok(backend) => tokio::time::timeout_at(deadline, backend.infer(request, deadline))
                    .await
                    .unwrap_or(Err(ModelError::Timeout)),
                Err(error) => Err(error),
            };
            pending.completed = true;
            let observed = match &result {
                Ok(reply) => json!({"status":"parsed", "reply":reply}),
                Err(error) => json!({"status":"error", "error":error}),
            };
            if self.record.save("model_result", &observed).is_err() {
                self.recording_failed.store(true, Ordering::Relaxed);
                return Err(ModelError::InvalidResponse);
            }
            result
        })
    }
}

/// Advisory recommendation surfaced to AW when the step requests a decision.
pub(crate) struct Advisory {
    pub decision: &'static str,
    pub obligations: Vec<Value>,
}

/// Maps the composed outcome to the AW decision vocabulary. An absent judgment
/// (nop) abstains and, per protocol, cannot request obligations.
fn advisory(response: &Value) -> Advisory {
    let decision = match response["outcome"]["control"]["candidate"].as_str() {
        Some("allow") => "allow",
        Some("ask") => "ask",
        Some("block") => "block",
        _ => "abstain",
    };
    let obligations = if decision == "abstain" {
        vec![]
    } else {
        response["outcome"]["obligations"]
            .as_array()
            .map_or(vec![], |items| {
                items
                    .iter()
                    .filter_map(|o| o.get("action").cloned())
                    .collect()
            })
    };
    Advisory {
        decision,
        obligations,
    }
}

pub(crate) async fn invoke(
    request: &Value,
    config: Config,
    ingress: Ingress,
) -> (&'static str, Option<Advisory>) {
    if request["event"]["name"] == "input.submit" {
        return (session::capture(request, &config, ingress), None);
    }
    let budget = match mapping::budget(request, &config, ingress) {
        Ok(budget) => budget,
        Err(code) => return (code, None),
    };
    let deadline = ingress.deadline_at(budget.deadline_ms, config.model.timeout_ms);
    // A call without a valid scoped session is still retained, in the unbound log.
    let session = mapping::context(request, &config, "tool.before").ok();
    let record = match Record::create(&config, session.as_deref(), deadline) {
        Ok(record) => Arc::new(record),
        Err(code) => return (code, None),
    };
    if let Err(code) = record.save("input", &json!({
        "mode":"private_local_shadow", "filesystem_attested":false,
        "aw_request":request, "ingress_unix_ms":ingress.now_ms().saturating_sub(ingress.elapsed_ms())
    })) { return (code, None); }
    let result = run(request, config, ingress, record.clone()).await;
    let (status, code, response) = match result {
        Ok(response) => ("evaluated", "shadow_evaluated", Some(response)),
        Err(code) => (
            if [
                "context_mismatch",
                "missing_identity",
                "unmapped_tool",
                "unsupported_input",
                "unmapped_resource",
                "unsupported_operation",
                "oversized_candidate",
                "candidate_invalid",
                "task_unavailable",
                "task_too_large",
            ]
            .contains(&code)
            {
                "uncovered"
            } else {
                "skipped"
            },
            code,
            None,
        ),
    };
    let recommendation = response.as_ref().map(advisory);
    match record.save(
        "result",
        &json!({"status":status,"code":code,"evaluation":response}),
    ) {
        Ok(()) => (code, recommendation),
        Err(error) => (error, None),
    }
}

async fn run(
    request: &Value,
    mut config: Config,
    ingress: Ingress,
    record: Arc<Record>,
) -> Result<Value, &'static str> {
    // Mapping checks context before the public candidate revision size limit;
    // uncovered contexts stay uncovered even when their payload is oversized.
    let mut captured = None;
    let (mut evaluation, turns) = mapping::project(request, &config, ingress, |_| {
        let stored = record.session_task()?;
        let task = stored.task()?;
        captured = Some(stored);
        Ok(task)
    })?;
    let mut local = observation::collect(
        &evaluation.candidate,
        mapping::evidence_targets(&evaluation.candidate, &config),
        &config.workspace_path,
        captured.as_ref().and_then(|task| task.inventory.as_ref()),
        &evaluation.binding.candidate_revision,
        ingress,
        evaluation.budget.deadline_ms,
    );
    local.signals = signals::observe(&evaluation.candidate, &config.workspace_path);
    evaluation.local_context = Some(local);
    let evidence = collect_evidence(&mut evaluation, &config, ingress).await;
    let encoded = serde_json::to_vec(&evaluation).map_err(|_| "mapping_failed")?;
    if encoded.len() > semantic_arbiter::protocol::MAX_MESSAGE_BYTES {
        return Err("oversized_candidate");
    }
    record.save(
        "task",
        &json!({
            "origin":"native_user",
            "native_session_id":request["event"]["session_id"],
            "session_id":evaluation.binding.session_id,
            "adapter":request["event"]["agent"]["adapter"],
            "binding_id":request["event"]["agent"]["binding_id"],
            "instance_id":request["event"]["agent"]["instance_id"],
            "config_revision":evaluation.binding.config_revision,
            "task_version":evaluation.binding.task_version, "turns":turns,
            "text_bytes":evaluation.task.as_ref().map_or(0, |task| task.text.len())
        }),
    )?;
    // Every call records why evidence was or was not collected, so a missing
    // stage can never be confused with a silently skipped collection.
    record.save(
        "evidence",
        &evidence.unwrap_or_else(|| json!({"status": "scenario_gated"})),
    )?;
    record.save("evaluation", &evaluation)?;
    config.model.config_revision = evaluation.binding.config_revision.clone();
    if ingress.expired(&evaluation.budget, config.model.timeout_ms) {
        return Err("budget_exceeded");
    }
    let failed = Arc::new(AtomicBool::new(false));
    let backend = RecordingBackend {
        config: config.model.clone(),
        record,
        recording_failed: failed.clone(),
    };
    let evaluator =
        Evaluator::new(config.model.clone(), backend).map_err(|_| "evaluation_invalid")?;
    let run = evaluator
        .evaluate_from_ingress(&evaluation, ingress)
        .await
        .map_err(|_| "evaluation_invalid")?;
    if failed.load(Ordering::Relaxed) {
        return Err("record_write_failed");
    }
    validate_response(&run.response, &evaluation, &config.model, run.record())
        .map_err(|_| "evaluation_invalid")?;
    serde_json::to_value(run.response).map_err(|_| "evaluation_invalid")
}

/// Retains Git rebuild facts or explicit gaps for local policy and semantic fallback.
async fn collect_evidence(
    evaluation: &mut EvaluationRequest,
    config: &Config,
    ingress: Ingress,
) -> Option<Value> {
    if !mapping::needs_evidence(&evaluation.scenarios) {
        return None;
    }
    // A read-only candidate names no resource, so no rebuild fact is required
    // and no coverage gap exists; inventing one would misreport the call.
    if evaluation.candidate.changes.is_empty() {
        return Some(json!({"source": SOURCE_ID, "status": "no_targets"}));
    }
    let fallback: Vec<EvidenceGap> = [Scenario::Preservation, Scenario::Snapshot]
        .into_iter()
        .filter(|scenario| evaluation.scenarios.contains(scenario))
        .map(|scenario| EvidenceGap {
            source_id: SOURCE_ID.into(),
            scenario,
        })
        .collect();
    let targets = mapping::evidence_targets(&evaluation.candidate, config);
    let pairs: Vec<(&str, &str)> = targets
        .iter()
        .flatten()
        .map(|(id, path)| (id.as_str(), path.as_str()))
        .collect();
    let source = targets
        .as_ref()
        .and_then(|_| GitRebuildSource::new(&config.workspace_path, &pairs, ingress));
    let Some(source) = source else {
        evaluation.gaps = fallback;
        return Some(json!({"source": SOURCE_ID, "status": "unbound"}));
    };
    let mut registry = EvidenceSourceRegistry::new();
    let collected = match registry.register(source) {
        Ok(()) => {
            registry
                .collect(
                    &[SOURCE_ID.into()],
                    EvidenceRequest::from(&*evaluation),
                    ingress,
                )
                .await
        }
        Err(error) => Err(error),
    };
    match collected {
        Ok(collection) => {
            let summary = json!({
                "source": SOURCE_ID, "status": "collected",
                "collected_at_ms": collection.collected_at_ms, "reports": collection.reports
            });
            evaluation.evidence = collection.evidence;
            evaluation.gaps = collection.gaps;
            Some(summary)
        }
        Err(error) => {
            evaluation.gaps = fallback;
            Some(json!({"source": SOURCE_ID, "status": "failed", "error": error}))
        }
    }
}
