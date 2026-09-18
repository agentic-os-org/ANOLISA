//! Action-capability request parameters.
//!
//! These are untrusted wire values only. The code scanner's own types
//! (`Language`, `ScanResult`) live in the capability crate; the protocol layer
//! carries the language as a bare string so an unknown value is projected as a
//! clean `invalid_argument` by the handler rather than a serde decode error.

use serde::{Deserialize, Serialize};

/// Parameters for `action.code_scan`.
///
/// `rules`, when present, narrows the active rule set to the listed ids; a
/// missing value runs the whole set for the language. `mode` selects the
/// engine and defaults to `regex`; the daemon build carries only the regex
/// engine, so `llm` returns an engine-unavailable verdict rather than failing
/// the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeScanParams {
    /// Snippet to scan; empty input yields an error verdict, not a decode error.
    pub code: String,
    /// Language name, validated by the handler against the supported set.
    pub language: String,
    /// Optional allowlist of rule ids to run.
    #[serde(default)]
    pub rules: Option<Vec<String>>,
    /// Engine mode; defaults to `regex` when absent.
    #[serde(default)]
    pub mode: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_params_default_rules_and_mode_to_none() {
        let params: CodeScanParams =
            serde_json::from_value(serde_json::json!({"code": "echo hi", "language": "bash"}))
                .expect("minimal params decode");
        assert_eq!(params.code, "echo hi");
        assert_eq!(params.language, "bash");
        assert_eq!(params.rules, None);
        assert_eq!(params.mode, None);
    }

    #[test]
    fn full_params_round_trip() {
        let params: CodeScanParams = serde_json::from_value(serde_json::json!({
            "code": "rm -rf /",
            "language": "bash",
            "rules": ["shell-recursive-delete"],
            "mode": "regex",
        }))
        .expect("full params decode");
        assert_eq!(
            params.rules.as_deref(),
            Some(&["shell-recursive-delete".to_owned()][..])
        );
        assert_eq!(params.mode.as_deref(), Some("regex"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let decoded = serde_json::from_value::<CodeScanParams>(serde_json::json!({
            "code": "x",
            "language": "bash",
            "extra": true,
        }));
        assert!(decoded.is_err(), "unknown fields must be rejected");
    }
}
