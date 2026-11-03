//! Error types for the prompt scanner.

use thiserror::Error;

/// Errors raised by the prompt scanner pipeline.
#[derive(Debug, Error)]
pub enum ScannerError {
    /// Input text is invalid (e.g. empty after stripping whitespace).
    #[error("invalid scanner input: {0}")]
    Input(String),

    /// Scanner configuration is invalid (unknown detector, malformed
    /// built-in rule file, unknown scan mode, unsupported model name).
    #[error("invalid scanner configuration: {0}")]
    Config(String),

    /// A mandatory detection layer's dependencies are missing, so the
    /// scanner cannot be constructed.
    #[error("detection layer unavailable: {0}")]
    LayerNotAvailable(String),

    /// The configured model is not served by the inference backend
    /// (e.g. it was never pulled into Ollama).
    #[error("model unavailable: {0}")]
    ModelLoad(String),

    /// Inference failed: the service is unreachable or returned an
    /// unusable response.
    #[error("model inference failed: {0}")]
    ModelInference(String),

    /// Wraps an upstream [`asc_model_client::ModelServiceError`]; produced by
    /// `?` propagation from the shared model service client.
    #[error("model service error: {0}")]
    ModelService(#[from] asc_model_client::ModelServiceError),
}

#[cfg(test)]
mod tests {
    use asc_model_client::{ConfigError, ModelServiceError};

    use super::ScannerError;

    #[test]
    fn model_service_config_errors_keep_the_v1_rust_display() {
        for (config, expected) in [
            (
                ConfigError::UnsupportedBackend("bogus".to_owned()),
                "model service error: invalid model service configuration: Unsupported model service backend: \"bogus\"",
            ),
            (
                ConfigError::InvalidBaseUrl {
                    base_url: "http://[".to_owned(),
                    reason: "invalid IPv6 address".to_owned(),
                },
                "model service error: invalid model service configuration: base_url is not a valid URL \"http://[\": invalid IPv6 address",
            ),
            (
                ConfigError::UnsupportedScheme("ftp://localhost:11434".to_owned()),
                "model service error: invalid model service configuration: base_url must use http:// or https:// scheme: \"ftp://localhost:11434\"",
            ),
            (
                ConfigError::NonLoopbackBaseUrl("http://10.0.0.1:11434".to_owned()),
                "model service error: invalid model service configuration: refusing non-loopback model service base_url \"http://10.0.0.1:11434\": only a local model service is supported, and scanned prompts must not leave the host",
            ),
        ] {
            let error = ScannerError::from(ModelServiceError::Config(config));
            assert_eq!(error.to_string(), expected);
        }
    }
}
