//! Persist model-bound input before network I/O; expose only advisory local results.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{validate_response, Evaluator};
use semantic_arbiter::model::{
    http::HttpBackend, BackendIdentity, ModelBackend, ModelError, ModelReply, ModelRequest,
};
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::{config::Config, mapping, session, storage::Record};

struct RecordingBackend {
    backend: HttpBackend,
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
        self.backend.identity()
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
            let result = tokio::time::timeout_at(deadline, self.backend.infer(request, deadline))
                .await
                .unwrap_or(Err(ModelError::Timeout));
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

pub(crate) async fn invoke(request: &Value, config: Config, ingress: Ingress) -> &'static str {
    if request["event"]["name"] == "input.submit" {
        return session::capture(request, &config, ingress);
    }
    let budget = match mapping::budget(request, &config, ingress) {
        Ok(budget) => budget,
        Err(code) => return code,
    };
    let deadline = ingress.deadline_at(budget.deadline_ms, config.model.timeout_ms);
    // A call without a valid scoped session is still retained, in the unbound log.
    let session = mapping::context(request, &config, "tool.before").ok();
    let record = match Record::create(&config, session.as_deref(), deadline) {
        Ok(record) => Arc::new(record),
        Err(code) => return code,
    };
    if let Err(code) = record.save("input", &json!({
        "version":1, "mode":"private_local_shadow", "filesystem_attested":false,
        "aw_request":request, "ingress_unix_ms":ingress.now_ms().saturating_sub(ingress.elapsed_ms())
    })) { return code; }
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
    match record.save(
        "result",
        &json!({"status":status,"code":code,"evaluation":response}),
    ) {
        Ok(()) => code,
        Err(error) => error,
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
    let (evaluation, turns) =
        mapping::project(request, &config, ingress, |_| record.session_task()?.task())?;
    let encoded = serde_json::to_vec(&evaluation).map_err(|_| "mapping_failed")?;
    if encoded.len() > semantic_arbiter::protocol::MAX_MESSAGE_BYTES {
        return Err("oversized_candidate");
    }
    record.save(
        "task",
        &json!({
            "version":1, "origin":"native_user",
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
    record.save("evaluation", &evaluation)?;
    config.model.config_revision = evaluation.binding.config_revision.clone();
    if ingress.expired(&evaluation.budget, config.model.timeout_ms) {
        return Err("budget_exceeded");
    }
    let backend = HttpBackend::from_env(&config.model).map_err(model_code)?;
    let failed = Arc::new(AtomicBool::new(false));
    let backend = RecordingBackend {
        backend,
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

fn model_code(error: ModelError) -> &'static str {
    match error {
        ModelError::MissingCredentials => "missing_credentials",
        ModelError::Timeout => "budget_exceeded",
        _ => "model_setup_failed",
    }
}
