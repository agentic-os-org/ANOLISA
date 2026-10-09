//! Action-runtime adapter for the code scanner.

use std::sync::Arc;

use asc_action_runtime::{CapabilityExecutor, ExecutionControl};
use asc_action_types::ActionOutcome;
use asc_model_client::{
    GenerateRequest, ModelClient, ModelOptions, ModelServiceError, create_client,
};
use serde_json::{Map, Value};

use crate::{Language, scan};

pub use asc_action_types::CodeScanRequest;

/// Executes code scans through the shared action runtime.
#[derive(Clone)]
pub struct CodeScanExecutor {
    model_client: Option<Arc<dyn ModelClient>>,
    initialization_error: Option<String>,
}

impl CodeScanExecutor {
    /// Creates an executor with an injected local-model client.
    #[must_use]
    pub fn new(model_client: Arc<dyn ModelClient>) -> Self {
        Self {
            model_client: Some(model_client),
            initialization_error: None,
        }
    }

    /// Creates the production executor from the shared local-model configuration.
    #[must_use]
    pub fn from_env() -> Self {
        match create_client() {
            Ok(client) => Self::new(Arc::from(client)),
            Err(error) => Self {
                model_client: None,
                initialization_error: Some(crate::llm::initialization_message(&error)),
            },
        }
    }
}

impl Default for CodeScanExecutor {
    fn default() -> Self {
        Self::new(Arc::new(UnavailableModelClient))
    }
}

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
        let result = if mode == "llm" {
            match (&self.model_client, &self.initialization_error) {
                (Some(model_client), _) => {
                    crate::llm::scan(&request.code, language, model_client.as_ref())
                }
                (None, Some(error)) => crate::llm::unavailable(&request.code, language, error),
                (None, None) => {
                    unreachable!("model client absence carries its initialization error")
                }
            }
        } else {
            scan(&request.code, language, request.rules.as_deref(), mode)
        };
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

struct UnavailableModelClient;

impl ModelClient for UnavailableModelClient {
    fn check_model(&self, _: &str) -> bool {
        false
    }

    fn generate(&self, _: &GenerateRequest<'_>) -> Result<Value, ModelServiceError> {
        Err(ModelServiceError::Inference(
            "model client unavailable".to_owned(),
        ))
    }

    fn chat(
        &self,
        _: &str,
        _: &[(&str, &str)],
        _: &ModelOptions,
        _: bool,
        _: u32,
    ) -> Result<Value, ModelServiceError> {
        Err(ModelServiceError::Inference(
            "model client unavailable".to_owned(),
        ))
    }
}
