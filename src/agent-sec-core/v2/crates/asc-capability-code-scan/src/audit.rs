//! v1-compatible audit projection for code scans.

use asc_action_runtime::AuditProjector;
use asc_action_types::{ActionOutcome, AuditProjection, Failure};
use serde_json::{Map, Value};

use crate::CodeScanRequest;

/// Projects code-scan inputs and outputs into v1 `SecurityEvent.details`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodeScanAuditProjector;

impl AuditProjector for CodeScanAuditProjector {
    type Request = CodeScanRequest;

    fn project(&self, request: &CodeScanRequest, outcome: &ActionOutcome) -> AuditProjection {
        let mut audited_request = Map::new();
        audited_request.insert("code".to_owned(), Value::String(request.code.clone()));
        audited_request.insert(
            "language".to_owned(),
            Value::String(request.language.clone()),
        );
        if let Some(rules) = &request.rules {
            audited_request.insert(
                "rules".to_owned(),
                Value::Array(rules.iter().cloned().map(Value::String).collect()),
            );
        }
        if let Some(mode) = &request.mode {
            audited_request.insert("mode".to_owned(), Value::String(mode.clone()));
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
    fn preserves_v1_request_and_result_shapes() {
        let outcome = ActionOutcome {
            success: true,
            exit_code: 0,
            error: None,
            error_type: String::new(),
            data: Map::from_iter([(String::from("verdict"), json!("pass"))]),
        };
        let projection = CodeScanAuditProjector.project(
            &CodeScanRequest {
                code: "echo hi".to_owned(),
                language: "bash".to_owned(),
                rules: None,
                mode: None,
            },
            &outcome,
        );
        assert_eq!(
            projection.into_details(),
            json!({"request": {"code": "echo hi", "language": "bash"}, "result": {"verdict": "pass"}})
                .as_object()
                .expect("object")
                .clone()
        );
    }
}
