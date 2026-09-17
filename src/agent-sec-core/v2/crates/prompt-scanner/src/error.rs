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

    /// Wraps an upstream [`model_service::ModelServiceError`]; produced by
    /// `?` propagation from the shared model service client.
    #[error("model service error: {0}")]
    ModelService(#[from] model_service::ModelServiceError),
}
