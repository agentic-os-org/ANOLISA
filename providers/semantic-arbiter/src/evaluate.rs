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

use crate::{config::Config, mapping, storage::Record};

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
                    "30-model-result.json",
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
            if self.record.save("20-model-request.json", &json!({
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
            if self.record.save("30-model-result.json", &observed).is_err() {
                self.recording_failed.store(true, Ordering::Relaxed);
                return Err(ModelError::InvalidResponse);
            }
            result
        })
    }
}

pub(crate) async fn invoke(request: &Value, config: Config, ingress: Ingress) -> &'static str {
    let record = match Record::create(&config) {
        Ok(record) => Arc::new(record),
        Err(code) => return code,
    };
    if let Err(code) = record.save("00-input.json", &json!({
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
        "40-result.json",
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
    let evaluation = mapping::project(request, &config, ingress)?;
    let encoded = serde_json::to_vec(&evaluation).map_err(|_| "mapping_failed")?;
    if encoded.len() > semantic_arbiter::protocol::MAX_MESSAGE_BYTES {
        return Err("oversized_candidate");
    }
    record.save("10-evaluation.json", &evaluation)?;
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
