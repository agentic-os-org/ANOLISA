//! Action-runtime adapter for the code scanner.

use asc_action_runtime::{CapabilityExecutor, ExecutionControl};
use asc_action_types::ActionOutcome;
use serde_json::{Map, Value};

use crate::{Language, scan};

/// Request accepted by the code-scan capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeScanRequest {
    /// Code supplied by the caller.
    pub code: String,
    /// Language literal supplied by the caller.
    pub language: String,
    /// Optional selected rule IDs.
    pub rules: Option<Vec<String>>,
    /// Optional engine mode.
    pub mode: Option<String>,
}

/// Executes code scans through the shared action runtime.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodeScanExecutor;

impl CapabilityExecutor for CodeScanExecutor {
    type Request = CodeScanRequest;

    fn execute(&self, _: &ExecutionControl, request: &CodeScanRequest) -> ActionOutcome {
        let language = match Language::parse(&request.language) {
            Ok(language) => language,
            Err(error) => {
                return ActionOutcome {
                    success: false,
                    exit_code: 1,
                    error: Some(format!("scan error: {error}")),
                    error_type: "ErrUnsupportedLang".to_owned(),
                    data: Map::new(),
                };
            }
        };
        let mode = request.mode.as_deref().unwrap_or("regex");
        let result = scan(&request.code, language, request.rules.as_deref(), mode);
        let data = serde_json::to_value(&result)
            .expect("ScanResult is an owned serializable response shape");
        let Value::Object(data) = data else {
            unreachable!("ScanResult serializes to an object")
        };
        let success = result.ok;
        ActionOutcome {
            success,
            exit_code: i64::from(!success),
            error: None,
            error_type: if success {
                String::new()
            } else {
                "CodeScanError".to_owned()
            },
            data,
        }
    }
}
