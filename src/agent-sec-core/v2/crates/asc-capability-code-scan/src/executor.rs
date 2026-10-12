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
        Self::from_client(create_client())
    }

    fn from_client(client: Result<Box<dyn ModelClient>, ModelServiceError>) -> Self {
        match client {
            Ok(client) => Self::new(Arc::from(client)),
            // V1 builds a client for this URL and reports it as an unavailable model.
            Err(error) if crate::llm::v1_accepts_unparsable_loopback(&error) => {
                Self::new(Arc::new(UnavailableModelClient))
            }
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use asc_model_client::ConfigError;

    use super::*;

    fn llm_summary(executor: &CodeScanExecutor, code: &str) -> Value {
        let control = ExecutionControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: false,
        };
        let request = CodeScanRequest {
            code: code.to_owned(),
            language: "bash".to_owned(),
            rules: None,
            mode: Some("llm".to_owned()),
        };
        executor.execute(&control, &request).data["summary"].clone()
    }

    fn invalid_base_url(base_url: &str) -> ModelServiceError {
        ModelServiceError::Config(ConfigError::InvalidBaseUrl {
            base_url: base_url.to_owned(),
            reason: "invalid port number".to_owned(),
        })
    }

    #[test]
    fn malformed_loopback_port_reports_the_v1_unavailable_model() {
        let executor =
            CodeScanExecutor::from_client(Err(invalid_base_url("http://localhost:notaport")));

        assert_eq!(
            llm_summary(&executor, "echo hello"),
            "scan error: model 'warden' not available"
        );
        assert_eq!(llm_summary(&executor, " "), "scan error: empty input code");
    }

    #[test]
    fn malformed_remote_url_keeps_the_v1_initialization_error() {
        let executor = CodeScanExecutor::from_client(Err(invalid_base_url("http://exa mple")));

        assert_eq!(
            llm_summary(&executor, "echo hello"),
            "scan error: refusing non-loopback model service base_url 'http://exa mple': only \
             a local model service is supported, and scanned content must not leave the host"
        );
    }
}
