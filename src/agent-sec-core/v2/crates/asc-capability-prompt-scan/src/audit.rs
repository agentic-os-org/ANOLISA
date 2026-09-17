//! Audit projection for prompt scans.

use asc_action_runtime::AuditProjector;
use asc_action_types::{ActionOutcome, AuditProjection, Failure, PromptScanRequest};
use serde_json::{Map, Value};

/// Projects prompt-scan inputs and outputs into `SecurityEvent.details`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PromptScanAuditProjector;

impl AuditProjector for PromptScanAuditProjector {
    type Request = PromptScanRequest;

    fn project(&self, request: &PromptScanRequest, outcome: &ActionOutcome) -> AuditProjection {
        let mut audited_request = Map::new();
        audited_request.insert("text".to_owned(), Value::String(request.text.clone()));
        if let Some(mode) = &request.mode {
            audited_request.insert("mode".to_owned(), Value::String(mode.clone()));
        }
        if let Some(source) = &request.source {
            audited_request.insert("source".to_owned(), Value::String(source.clone()));
        }
        // The chosen L2 backend decides what judged the prompt, so it belongs
        // in the audit trail. `history` and `assistant_response` stay out:
        // like `text` they are the scanned input, and the triple's query is
        // already recorded, keeping events bounded on long conversations.
        if let Some(model) = &request.model {
            audited_request.insert("model".to_owned(), Value::String(model.clone()));
        }
        AuditProjection::Completed {
            request: audited_request,
            result: outcome.data.clone(),
            failure: (!outcome.success && !outcome.error_type.is_empty()).then(|| Failure {
                error: outcome.error.clone(),
                error_type: outcome.error_type.clone(),
                exit_code: outcome.exit_code,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use asc_action_runtime::AuditProjector;
    use serde_json::json;

    use super::*;

    #[test]
    fn preserves_request_and_result_shapes() {
        let outcome = ActionOutcome {
            success: true,
            exit_code: 0,
            error: None,
            error_type: String::new(),
            data: Map::from_iter([(String::from("verdict"), json!("pass"))]),
        };
        let projection = PromptScanAuditProjector.project(
            &PromptScanRequest {
                text: "hello".to_owned(),
                mode: Some("fast".to_owned()),
                source: Some("user_input".to_owned()),
                model: None,
                assistant_response: None,
                history: None,
            },
            &outcome,
        );
        assert_eq!(
            projection.into_details(),
            json!({
                "request": {"text": "hello", "mode": "fast", "source": "user_input"},
                "result": {"verdict": "pass"}
            })
            .as_object()
            .expect("object")
            .clone()
        );
    }

    #[test]
    fn optional_request_fields_are_omitted_when_absent() {
        let outcome = ActionOutcome {
            success: true,
            exit_code: 0,
            error: None,
            error_type: String::new(),
            data: Map::new(),
        };
        let projection = PromptScanAuditProjector.project(
            &PromptScanRequest {
                text: "hello".to_owned(),
                mode: None,
                source: None,
                model: None,
                assistant_response: None,
                history: None,
            },
            &outcome,
        );
        let details = projection.into_details();
        let request = details
            .get("request")
            .and_then(Value::as_object)
            .expect("request");
        assert_eq!(request.len(), 1);
        assert!(request.contains_key("text"));
    }

    #[test]
    fn failed_outcomes_append_failure_fields() {
        let outcome = ActionOutcome {
            success: false,
            exit_code: 1,
            error: Some("unsupported prompt scan mode: \"turbo\"".to_owned()),
            error_type: "ErrInvalidMode".to_owned(),
            data: Map::new(),
        };
        let details = PromptScanAuditProjector
            .project(
                &PromptScanRequest {
                    text: "hello".to_owned(),
                    mode: Some("turbo".to_owned()),
                    source: None,
                    model: Some("modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF".to_owned()),
                    assistant_response: None,
                    history: None,
                },
                &outcome,
            )
            .into_details();
        assert_eq!(
            details["error"],
            json!("unsupported prompt scan mode: \"turbo\"")
        );
        assert_eq!(details["error_type"], json!("ErrInvalidMode"));
        assert_eq!(details["exit_code"], json!(1));
        // The backend override reaches the audit trail even for a request the
        // scanner never ran, so post-incident review can see what was chosen.
        assert_eq!(
            details["request"]["model"],
            json!("modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF")
        );
    }
}
