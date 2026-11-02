//! Task-text intent proposal protocol; carries only the task text and a TTL, with no caller
//! path metadata, content snapshots, identity, or approval claims.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Full request budget (including newlines).
pub const INTENT_REQUEST_BYTES: usize = 256 * 1024;
/// Full result budget.
pub const INTENT_RESULT_BYTES: usize = 512 * 1024;
/// Task text budget.
pub const INTENT_TASK_BYTES: usize = 64 * 1024;

/// Semantic version of the direct Cedar generation request; mismatched requests must fail
/// explicitly.
pub const INTENT_REQUEST_VERSION: &str = "asc.intent.request.v1";

/// Carries the task the model receives; no caller path metadata is forwarded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentProposeParams {
    /// Required semantic version, validated by the server before the model call.
    pub format_version: String,
    /// Raw task; the model receives the task text and the shared schema only.
    pub task: String,
    /// Candidate TTL, 1..=1800 seconds.
    pub ttl: u32,
}

/// Direct Cedar candidate result; passing validation means neither task completeness nor an
/// approved policy.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntentProposeResult {
    /// A candidate that passed program checks; it may still carry model-reported unresolved items.
    Candidate {
        /// asc.intent.candidate.v1: raw Cedar draft, final artifact and source/integrity data.
        candidate: Value,
    },
    /// The paths or unsupported policy features need rework; there is no approvable candidate.
    NeedsClarification {
        /// Raw Cedar draft.
        draft: Value,
        /// Reasons reported by the program checks.
        reasons: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requires_version_and_rejects_undeclared_authority() {
        let valid = json!({"format_version":INTENT_REQUEST_VERSION, "task":"review", "ttl":30});
        assert!(serde_json::from_value::<IntentProposeParams>(valid.clone()).is_ok());
        let mut missing_version = valid.clone();
        missing_version
            .as_object_mut()
            .unwrap()
            .remove("format_version");
        assert!(serde_json::from_value::<IntentProposeParams>(missing_version).is_err());
        for key in [
            "trusted",
            "uid",
            "entity_id",
            "digest",
            "expires_at",
            "endpoint",
            "key",
            "filesystem_capabilities",
            "materials",
            "scopes",
            "workspace",
            "attachments",
            "protected_files",
        ] {
            let mut bad = valid.clone();
            bad.as_object_mut().unwrap().insert(key.into(), json!(true));
            assert!(serde_json::from_value::<IntentProposeParams>(bad).is_err());
        }
    }

    #[test]
    fn request_budget_counts_whitespace_escaping_context_and_line_ending() {
        use crate::request_fits_budget;
        let mut wire =
            br#"{"method":"intent.propose","params":{},"traceContext":{"version":1}}"#.to_vec();
        wire.resize(INTENT_REQUEST_BYTES - 1, b' ');
        assert!(request_fits_budget(&wire).unwrap());
        wire.push(b'\n');
        assert!(request_fits_budget(&wire).unwrap());
        wire.insert(0, b' ');
        assert!(!request_fits_budget(&wire).unwrap());
        let escaped = format!(
            r#"{{"method":"intent.propose","params":{{"task":"{}"}}}}"#,
            "\\u0001".repeat(INTENT_REQUEST_BYTES / 6)
        );
        assert!(!request_fits_budget(escaped.as_bytes()).unwrap());
        let mut other = br#"{"method":"health","params":{}}"#.to_vec();
        other.resize(INTENT_REQUEST_BYTES + 1, b' ');
        assert!(request_fits_budget(&other).unwrap());
    }

    #[test]
    fn outcomes_cannot_mix_candidates_and_rejections() {
        assert!(
            serde_json::from_value::<IntentProposeResult>(
                json!({"outcome":"needs_clarification","draft":{},"reasons":[],"candidate":{}})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<IntentProposeResult>(
                json!({"outcome":"rejected","draft":{},"reason":"unsupported_request"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<IntentProposeResult>(
                json!({"outcome":"approved","candidate":{}})
            )
            .is_err()
        );
    }
}
